use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::ContextError;
use crate::value::ContextValue;

/// Type alias for versioned deserializer functions.
type DeserializeFn =
    Box<dyn Fn(&[u8]) -> Result<Box<dyn ContextValue>, ContextError> + Send + Sync>;

/// Type alias for custom serializer functions (Arc so it can be cloned without a lock).
type SerializeFn = Arc<dyn Fn(&dyn ContextValue) -> Result<Vec<u8>, ContextError> + Send + Sync>;

pub(crate) type RegistryMap = HashMap<&'static str, Registration>;

/// Metadata stored for each registered context key.
pub(crate) struct Registration {
    pub key: &'static str,
    pub type_id: TypeId,
    /// The current (latest) version used for serialization.
    pub key_version: u32,
    /// Versioned deserializers: wire_version → deserializer function.
    pub deserializers: HashMap<u32, DeserializeFn>,
    pub type_name: &'static str,
    /// If true, this key is excluded from serialization.
    pub local_only: bool,
    /// Custom serializer. If None, uses ContextValue::serialize_value() (bincode).
    pub serialize_fn: Option<SerializeFn>,
    /// If true, the effective value is eagerly copied into each new scope
    /// on scope entry. This gives O(1) reads at the cost of an Arc::clone
    /// per scope entry. Suitable for lightweight values (request IDs, trace IDs).
    /// Default: false (reads walk the parent scope chain, O(depth)).
    pub cached: bool,
    /// Extensible metadata: any crate can attach typed metadata to a registration.
    /// Keyed by TypeId of the metadata type.
    pub metadata: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

// ── Two-phase storage ──────────────────────────────────────────
//
// Build phase  : RegistryBuilder collects registrations (no locks).
// Frozen phase : initialize(builder) moves them into FROZEN (OnceLock).
//                All subsequent reads are lock-free.
//
// Tests use BUILD (Mutex) via pub(crate) free-standing functions,
// so they work without calling initialize().

/// Immutable map used after `initialize()`. Lock-free reads.
static FROZEN: OnceLock<RegistryMap> = OnceLock::new();

/// Mutable map used by tests (via pub(crate) free-standing functions).
/// Not used in production — only a fallback when FROZEN is not set.
static BUILD: std::sync::LazyLock<Mutex<Option<RegistryMap>>> =
    std::sync::LazyLock::new(|| Mutex::new(Some(HashMap::new())));
static EMPTY_MAP: std::sync::LazyLock<RegistryMap> = std::sync::LazyLock::new(HashMap::new);

fn lock_build() -> std::sync::MutexGuard<'static, Option<RegistryMap>> {
    BUILD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read-only view into a registry map. Used for dependency injection.
pub(crate) struct Registry<'a> {
    map: &'a RegistryMap,
}

impl<'a> Registry<'a> {
    pub(crate) fn new(map: &'a RegistryMap) -> Self {
        Self { map }
    }

    pub(crate) fn empty() -> Registry<'static> {
        Registry { map: &EMPTY_MAP }
    }

    pub(crate) fn with_registration<R>(
        &self,
        key: &str,
        f: impl FnOnce(&Registration) -> R,
    ) -> Option<R> {
        self.map.get(key).map(f)
    }

    pub(crate) fn get_serialization_info(&self, key: &str) -> Option<SerializationInfo> {
        self.map.get(key).map(|r| SerializationInfo {
            key_version: r.key_version,
            serialize_fn: r.serialize_fn.clone(),
        })
    }

    pub(crate) fn cached_keys(&self) -> Vec<&'static str> {
        self.map
            .iter()
            .filter(|(_, r)| r.cached)
            .map(|(&k, _)| k)
            .collect()
    }

    pub(crate) fn is_local_key(&self, key: &str) -> bool {
        self.map.get(key).is_some_and(|r| r.local_only)
    }

    pub(crate) fn is_valid_value(&self, key: &str, value: &dyn ContextValue) -> bool {
        self.map
            .get(key)
            .is_some_and(|r| r.type_id == value.as_any().type_id())
    }

    pub(crate) fn with_metadata<M: 'static, R>(
        &self,
        key: &str,
        f: impl FnOnce(&M) -> R,
    ) -> Option<R> {
        self.with_registration(key, |r| {
            r.metadata
                .get(&TypeId::of::<M>())
                .and_then(|boxed| boxed.downcast_ref::<M>())
                .map(f)
        })
        .flatten()
    }

    pub(crate) fn keys_with_metadata<M: 'static, R>(
        &self,
        f: impl Fn(&'static str, &M) -> R,
    ) -> Vec<R> {
        self.map
            .iter()
            .filter_map(|(&key, reg)| {
                reg.metadata
                    .get(&TypeId::of::<M>())
                    .and_then(|boxed| boxed.downcast_ref::<M>())
                    .map(|meta| f(key, meta))
            })
            .collect()
    }
}

/// Execute `f` with a reference to the global registry.
/// After initialize(): lock-free. Before: acquires Mutex.
pub(crate) fn with_global_registry<R>(f: impl FnOnce(&Registry<'_>) -> R) -> R {
    if let Some(frozen) = FROZEN.get() {
        return f(&Registry::new(frozen));
    }

    let guard = lock_build();
    match guard.as_ref() {
        Some(map) => f(&Registry::new(map)),
        None => f(&Registry::empty()),
    }
}

// ── Registration options ───────────────────────────────────────

/// Builder for configuring per-key registration options.
///
/// Obtained via the callback in [`RegistryBuilder::try_register_with`].
///
/// # Examples
///
/// ```rust,ignore
/// builder.register_with::<Config>("config", |opts| opts
///     .version(1)
///     .codec(
///         |val| serde_json::to_vec(val).map_err(|e| e.to_string()),
///         |bytes| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
///     )
/// );
/// ```
#[allow(clippy::type_complexity)]
pub struct RegistrationOptions<T: 'static> {
    version: u32,
    local_only: bool,
    cached: bool,
    encode: Option<Box<dyn Fn(&T) -> Result<Vec<u8>, String> + Send + Sync>>,
    decode: Option<Box<dyn Fn(&[u8]) -> Result<T, String> + Send + Sync>>,
    metadata: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl<T: 'static> RegistrationOptions<T> {
    fn new() -> Self {
        Self {
            version: 1,
            local_only: false,
            cached: false,
            encode: None,
            decode: None,
            metadata: HashMap::new(),
        }
    }

    /// Set the wire format version (default: 1).
    pub fn version(mut self, v: u32) -> Self {
        self.version = v;
        self
    }

    /// Mark as local-only: propagates via snapshot/attach but excluded from
    /// serialization. The type does not need `Serialize`/`DeserializeOwned`.
    pub fn local_only(mut self) -> Self {
        self.local_only = true;
        self
    }

    /// Enable per-scope caching: the effective value is eagerly copied (Arc::clone)
    /// into each new scope on entry, giving O(1) reads. Best for lightweight values
    /// like request IDs or trace IDs. Without this, reads walk the parent scope
    /// chain (O(depth)).
    pub fn cached(mut self) -> Self {
        self.cached = true;
        self
    }

    /// Use a custom serialization codec instead of bincode.
    /// Both `encode` and `decode` must be provided together.
    pub fn codec(
        mut self,
        encode: impl Fn(&T) -> Result<Vec<u8>, String> + Send + Sync + 'static,
        decode: impl Fn(&[u8]) -> Result<T, String> + Send + Sync + 'static,
    ) -> Self {
        self.encode = Some(Box::new(encode));
        self.decode = Some(Box::new(decode));
        self
    }

    /// Attach typed metadata to this registration. Any crate can define its
    /// own metadata type and attach it here. Only one value per metadata type
    /// is stored; a second call with the same `M` overwrites the previous value.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use dcontext_tracing::LogField;
    ///
    /// builder.register_with::<RequestId>("request_id", |opts| {
    ///     opts.cached().with_metadata(LogField::display::<RequestId>("rid"))
    /// });
    /// ```
    pub fn with_metadata<M: Any + Send + Sync + 'static>(mut self, value: M) -> Self {
        self.metadata.insert(TypeId::of::<M>(), Box::new(value));
        self
    }
}

// ── Private implementation functions ───────────────────────────
//
// Shared logic used by both RegistryBuilder and free-standing test helpers.

fn do_register_with<T>(
    registry: &mut RegistryMap,
    key: &'static str,
    configure: impl FnOnce(RegistrationOptions<T>) -> RegistrationOptions<T>,
) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let opts = configure(RegistrationOptions::new());

    if opts.local_only {
        if opts.encode.is_some() || opts.decode.is_some() {
            return Err(ContextError::SerializationFailed(
                "local_only and codec are mutually exclusive: \
                 local-only entries are excluded from serialization"
                    .into(),
            ));
        }
        if opts.version != 1 {
            return Err(ContextError::SerializationFailed(
                "local_only and version are mutually exclusive: \
                 local-only entries have no wire format"
                    .into(),
            ));
        }
    }

    let tid = TypeId::of::<T>();

    if let Some(existing) = registry.get(key) {
        if existing.type_id == tid {
            return Ok(()); // idempotent
        }
        return Err(ContextError::AlreadyRegistered(key.to_string()));
    }

    let mut deserializers: HashMap<u32, DeserializeFn> = HashMap::new();

    if !opts.local_only {
        if let Some(decode) = opts.decode {
            deserializers.insert(
                opts.version,
                Box::new(
                    move |bytes: &[u8]| -> Result<Box<dyn ContextValue>, ContextError> {
                        decode(bytes)
                            .map(|v| Box::new(v) as Box<dyn ContextValue>)
                            .map_err(ContextError::DeserializationFailed)
                    },
                ),
            );
        } else {
            deserializers.insert(
                opts.version,
                Box::new(
                    |bytes: &[u8]| -> Result<Box<dyn ContextValue>, ContextError> {
                        bincode::deserialize::<T>(bytes)
                            .map(|v| Box::new(v) as Box<dyn ContextValue>)
                            .map_err(|e| ContextError::DeserializationFailed(e.to_string()))
                    },
                ),
            );
        }
    }

    let serialize_fn = opts.encode.map(|encode| -> SerializeFn {
        Arc::new(move |val: &dyn ContextValue| {
            let typed = val.as_any().downcast_ref::<T>().ok_or_else(|| {
                ContextError::SerializationFailed(
                    "type mismatch during custom serialization".into(),
                )
            })?;
            encode(typed).map_err(ContextError::SerializationFailed)
        })
    });

    registry.insert(
        key,
        Registration {
            key,
            type_id: tid,
            key_version: opts.version,
            deserializers,
            type_name: std::any::type_name::<T>(),
            local_only: opts.local_only,
            serialize_fn,
            cached: opts.cached,
            metadata: opts.metadata,
        },
    );
    Ok(())
}

fn do_register_migration<TOld, TCurrent>(
    registry: &mut RegistryMap,
    key: &'static str,
    old_version: u32,
    migrate: impl Fn(TOld) -> TCurrent + Send + Sync + 'static,
) -> Result<(), ContextError>
where
    TOld: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    TCurrent: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let reg = registry
        .get_mut(key)
        .ok_or_else(|| ContextError::NotRegistered(key.to_string()))?;

    if reg.type_id != TypeId::of::<TCurrent>() {
        return Err(ContextError::TypeMismatch(
            key.to_string(),
            reg.type_name.to_string(),
            std::any::type_name::<TCurrent>().to_string(),
        ));
    }

    if reg.local_only {
        return Err(ContextError::SerializationFailed(format!(
            "cannot register migration for local-only key '{}'",
            key
        )));
    }

    if old_version == reg.key_version {
        return Err(ContextError::DeserializationFailed(format!(
            "cannot register migration for key '{}' at current version {} \
             (would overwrite the native deserializer)",
            key, old_version
        )));
    }

    reg.deserializers.insert(
        old_version,
        Box::new(
            move |bytes: &[u8]| -> Result<Box<dyn ContextValue>, ContextError> {
                let old_val = bincode::deserialize::<TOld>(bytes)
                    .map_err(|e| ContextError::DeserializationFailed(e.to_string()))?;
                let current_val = migrate(old_val);
                Ok(Box::new(current_val) as Box<dyn ContextValue>)
            },
        ),
    );

    Ok(())
}

// ── RegistryBuilder ────────────────────────────────────────────

/// Collects context registrations during application startup.
///
/// Create a builder, register all context types, then call
/// [`initialize`] to freeze the registry for lock-free reads.
///
/// # Examples
///
/// ```rust,ignore
/// use dcontext::{RegistryBuilder, initialize};
///
/// let mut builder = RegistryBuilder::new();
/// builder.register::<RequestId>("request_id");
/// builder.register_with::<TraceV2>("trace", |o| o.version(2));
/// builder.register_migration::<TraceV1, TraceV2>("trace", 1, migrate_fn);
///
/// initialize(builder); // freeze — all reads lock-free after this
/// ```
pub struct RegistryBuilder {
    map: RegistryMap,
}

impl RegistryBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Register a context type with default options (version 1, bincode codec).
    ///
    /// # Panics
    ///
    /// Panics if the key is already registered. Use [`try_register`](Self::try_register)
    /// for a non-panicking alternative.
    pub fn register<T>(&mut self, key: &'static str)
    where
        T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        self.try_register::<T>(key)
            .unwrap_or_else(|e| panic!("RegistryBuilder::register failed for key '{key}': {e}"));
    }

    /// Register a context type. Returns Err on conflict.
    pub fn try_register<T>(&mut self, key: &'static str) -> Result<(), ContextError>
    where
        T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        do_register_with::<T>(&mut self.map, key, |opts| opts)
    }

    /// Register with custom options via builder callback.
    ///
    /// # Panics
    ///
    /// Panics on conflict. Use [`try_register_with`](Self::try_register_with)
    /// for a non-panicking alternative.
    pub fn register_with<T>(
        &mut self,
        key: &'static str,
        configure: impl FnOnce(RegistrationOptions<T>) -> RegistrationOptions<T>,
    ) where
        T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        self.try_register_with::<T>(key, configure)
            .unwrap_or_else(|e| {
                panic!("RegistryBuilder::register_with failed for key '{key}': {e}")
            });
    }

    /// Register with custom options. Returns Err on conflict.
    pub fn try_register_with<T>(
        &mut self,
        key: &'static str,
        configure: impl FnOnce(RegistrationOptions<T>) -> RegistrationOptions<T>,
    ) -> Result<(), ContextError>
    where
        T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        do_register_with(&mut self.map, key, configure)
    }

    /// Register a migration deserializer for an older wire version.
    ///
    /// # Panics
    ///
    /// Panics on conflict or if the key is not registered.
    /// Use [`try_register_migration`](Self::try_register_migration)
    /// for a non-panicking alternative.
    pub fn register_migration<TOld, TCurrent>(
        &mut self,
        key: &'static str,
        old_version: u32,
        migrate: impl Fn(TOld) -> TCurrent + Send + Sync + 'static,
    ) where
        TOld: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        TCurrent: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        self.try_register_migration::<TOld, TCurrent>(key, old_version, migrate)
            .unwrap_or_else(|e| {
                panic!("RegistryBuilder::register_migration failed for key '{key}': {e}")
            });
    }

    /// Register a migration. Returns Err on conflict or if key not found.
    pub fn try_register_migration<TOld, TCurrent>(
        &mut self,
        key: &'static str,
        old_version: u32,
        migrate: impl Fn(TOld) -> TCurrent + Send + Sync + 'static,
    ) -> Result<(), ContextError>
    where
        TOld: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        TCurrent: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        do_register_migration(&mut self.map, key, old_version, migrate)
    }
}

impl Default for RegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl RegistryBuilder {
    pub(crate) fn into_map(self) -> RegistryMap {
        self.map
    }
}

// ── Initialization ─────────────────────────────────────────────

/// Freeze the registry. Consumes the builder and makes all reads lock-free.
///
/// Call this once after all registrations, before any context operations.
///
/// # Panics
///
/// Panics if called more than once. Use [`try_initialize`] for a
/// non-panicking alternative.
///
/// ```rust,ignore
/// let mut builder = dcontext::RegistryBuilder::new();
/// builder.register::<RequestId>("request_id");
/// builder.register_with::<TraceV2>("trace", |o| o.version(2));
/// builder.register_migration::<TraceV1, TraceV2>("trace", 1, migrate_fn);
///
/// dcontext::initialize(builder); // freeze
/// ```
pub fn initialize(builder: RegistryBuilder) {
    try_initialize(builder).expect("dcontext::initialize called more than once");
}

/// Try to freeze the registry. Returns `Err` if already initialized.
pub fn try_initialize(builder: RegistryBuilder) -> Result<(), ContextError> {
    FROZEN
        .set(builder.map)
        .map_err(|_| ContextError::RegistryFrozen)
}

// ── Free-standing registration functions (for tests) ───────────
//
// Tests run in the same process and cannot call initialize() per-test
// (OnceLock is one-shot). These functions write to the BUILD mutex,
// and read functions fall back to BUILD when FROZEN is not set.

#[cfg(test)]
pub(crate) fn try_register<T>(key: &'static str) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register_with::<T>(key, |opts| opts)
}

#[cfg(test)]
pub(crate) fn try_register_with<T>(
    key: &'static str,
    configure: impl FnOnce(RegistrationOptions<T>) -> RegistrationOptions<T>,
) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let mut guard = lock_build();
    let registry = guard.as_mut().ok_or(ContextError::RegistryFrozen)?;
    do_register_with(registry, key, configure)
}

#[cfg(test)]
pub(crate) fn register<T>(key: &'static str)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register::<T>(key).expect("dcontext::register failed");
}

#[cfg(test)]
pub(crate) fn register_with<T>(
    key: &'static str,
    configure: impl FnOnce(RegistrationOptions<T>) -> RegistrationOptions<T>,
) where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register_with::<T>(key, configure).expect("dcontext::register_with failed");
}

#[cfg(test)]
pub(crate) fn try_register_migration<TOld, TCurrent>(
    key: &'static str,
    old_version: u32,
    migrate: impl Fn(TOld) -> TCurrent + Send + Sync + 'static,
) -> Result<(), ContextError>
where
    TOld: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    TCurrent: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let mut guard = lock_build();
    let registry = guard.as_mut().ok_or(ContextError::RegistryFrozen)?;
    do_register_migration(registry, key, old_version, migrate)
}

#[cfg(test)]
pub(crate) fn register_migration<TOld, TCurrent>(
    key: &'static str,
    old_version: u32,
    migrate: impl Fn(TOld) -> TCurrent + Send + Sync + 'static,
) where
    TOld: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    TCurrent: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register_migration::<TOld, TCurrent>(key, old_version, migrate)
        .expect("dcontext::register_migration failed");
}

// ── Read functions (lock-free after initialize) ────────────────

/// Look up a registration by key. Returns None if not registered.
///
/// After [`initialize`]: lock-free (OnceLock deref + HashMap lookup).
/// Before [`initialize`]: acquires Mutex (correct, but slower — for tests).
#[allow(dead_code)]
pub(crate) fn with_registration<R>(key: &str, f: impl FnOnce(&Registration) -> R) -> Option<R> {
    with_global_registry(|registry| registry.with_registration(key, f))
}

/// Info needed by `serialize_context`, fetched in a single lookup.
pub(crate) struct SerializationInfo {
    pub key_version: u32,
    pub serialize_fn: Option<SerializeFn>,
}

/// Single-lookup extraction of everything `serialize_context` needs.
#[allow(dead_code)]
pub(crate) fn get_serialization_info(key: &str) -> Option<SerializationInfo> {
    with_global_registry(|registry| registry.get_serialization_info(key))
}

/// Return registered keys that have per-scope caching enabled.
/// These keys will have their effective values eagerly copied into each
/// new scope on entry, giving O(1) reads.
#[allow(dead_code)]
pub(crate) fn cached_keys() -> Vec<&'static str> {
    with_global_registry(|registry| registry.cached_keys())
}

/// Check if a key is registered as local-only.
#[allow(dead_code)]
pub(crate) fn is_local_key(key: &str) -> bool {
    with_global_registry(|registry| registry.is_local_key(key))
}

/// Check if a value is valid for a given key (key exists and TypeId matches).
#[allow(dead_code)]
pub(crate) fn is_valid_value(key: &str, value: &dyn ContextValue) -> bool {
    with_global_registry(|registry| registry.is_valid_value(key, value))
}

// ── Metadata query API ─────────────────────────────────────────

/// Access typed metadata for a registered key via callback.
///
/// Returns `None` if the key is not registered or has no metadata of type `M`.
/// After [`initialize`]: lock-free. Before: acquires Mutex.
pub fn with_metadata<M: 'static, R>(key: &str, f: impl FnOnce(&M) -> R) -> Option<R> {
    with_global_registry(|registry| registry.with_metadata(key, f))
}

/// Iterate over all registered keys that have metadata of type `M`.
///
/// Calls `f(key, metadata)` for each matching key and collects the results.
/// After [`initialize`]: lock-free. Before: acquires Mutex.
pub fn keys_with_metadata<M: 'static, R>(f: impl Fn(&'static str, &M) -> R) -> Vec<R> {
    with_global_registry(|registry| registry.keys_with_metadata(f))
}

#[cfg(test)]
pub(crate) fn is_registered(key: &str) -> bool {
    if let Some(frozen) = FROZEN.get() {
        return frozen.contains_key(key);
    }
    let guard = lock_build();
    guard.as_ref().map_or(false, |map| map.contains_key(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Default, Debug, Serialize, Deserialize)]
    struct TestVal(String);

    #[derive(Clone, Default, Debug, Serialize, Deserialize)]
    struct OtherVal(u64);

    fn unique_reg_key(name: &str) -> &'static str {
        let s = format!("reg_test_{}", name);
        Box::leak(s.into_boxed_str())
    }

    #[test]
    fn register_and_lookup() {
        let key = unique_reg_key("lookup");
        try_register::<TestVal>(key).unwrap();
        assert!(is_registered(key));
        assert!(!is_registered("reg_test_missing_xxx"));
    }

    #[test]
    fn idempotent_registration() {
        let key = unique_reg_key("idem");
        try_register::<TestVal>(key).unwrap();
        try_register::<TestVal>(key).unwrap();
    }

    #[test]
    fn conflicting_registration() {
        let key = unique_reg_key("conflict");
        try_register::<TestVal>(key).unwrap();
        let err = try_register::<OtherVal>(key).unwrap_err();
        assert!(matches!(err, ContextError::AlreadyRegistered(_)));
    }

    #[test]
    fn registry_supports_injected_builder_map() {
        let key = unique_reg_key("injected");
        let mut builder = RegistryBuilder::new();
        builder.register_with::<TestVal>(key, |opts| opts.cached().with_metadata(7usize));

        let map = builder.into_map();
        let registry = Registry::new(&map);

        assert!(registry.with_registration(key, |_| true).unwrap_or(false));
        assert_eq!(registry.cached_keys(), vec![key]);
        assert!(registry.is_valid_value(key, &TestVal::default()));
        assert_eq!(registry.with_metadata::<usize, _>(key, |n| *n), Some(7));
    }
}
