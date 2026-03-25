use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::error::ContextError;
use crate::value::ContextValue;

/// Type alias for versioned deserializer functions.
type DeserializeFn = Box<dyn Fn(&[u8]) -> Result<Box<dyn ContextValue>, ContextError> + Send + Sync>;

/// Type alias for custom serializer functions (Arc so it can be cloned out of the lock).
type SerializeFn = Arc<dyn Fn(&dyn ContextValue) -> Result<Vec<u8>, ContextError> + Send + Sync>;

/// Metadata stored for each registered context key.
pub(crate) struct Registration {
    pub key: &'static str,
    pub type_id: TypeId,
    /// The current (latest) version used for serialization.
    pub key_version: u32,
    /// Versioned deserializers: wire_version → deserializer function.
    /// Each function deserializes bytes from that specific wire version
    /// into the current type (possibly via migration/conversion).
    pub deserializers: HashMap<u32, DeserializeFn>,
    pub type_name: &'static str,
    /// If true, this key is excluded from serialization.
    pub local_only: bool,
    /// Custom serializer. If None, uses ContextValue::serialize_value() (bincode).
    pub serialize_fn: Option<SerializeFn>,
}

static REGISTRY: std::sync::LazyLock<RwLock<HashMap<&'static str, Registration>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

/// Builder for configuring context registration options.
///
/// Obtained via the callback in [`try_register_with`] / [`register_with`].
///
/// # Examples
///
/// ```rust,ignore
/// // Versioned registration
/// register_with::<TraceV2>("trace", |opts| opts.version(2));
///
/// // Local-only (non-serializable)
/// register_with::<DbPool>("pool", |opts| opts.local_only());
///
/// // Custom codec (JSON instead of bincode)
/// register_with::<Config>("config", |opts| opts
///     .version(1)
///     .codec(
///         |val| serde_json::to_vec(val).map_err(|e| e.to_string()),
///         |bytes| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
///     )
/// );
/// ```
pub struct RegistrationOptions<T: 'static> {
    version: u32,
    local_only: bool,
    encode: Option<Box<dyn Fn(&T) -> Result<Vec<u8>, String> + Send + Sync>>,
    decode: Option<Box<dyn Fn(&[u8]) -> Result<T, String> + Send + Sync>>,
}

impl<T: 'static> RegistrationOptions<T> {
    fn new() -> Self {
        Self {
            version: 1,
            local_only: false,
            encode: None,
            decode: None,
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
}

/// Register a context type with default options (version 1, bincode codec).
/// Idempotent if same key+type; errors if key registered with a different type.
pub fn try_register<T>(key: &'static str) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register_with::<T>(key, |opts| opts)
}

/// Register a context type with custom options via builder callback.
///
/// # Examples
///
/// ```rust,ignore
/// // Versioned
/// try_register_with::<TraceV2>("trace", |opts| opts.version(2))?;
///
/// // Local-only (no Serialize needed at the call site, but T still must
/// // implement Serialize for the generic bound — use register_local for
/// // types that truly don't implement Serialize)
/// try_register_with::<Flags>("flags", |opts| opts.local_only())?;
///
/// // Custom codec
/// try_register_with::<Config>("config", |opts| opts
///     .codec(json_encode, json_decode)
/// )?;
/// ```
pub fn try_register_with<T>(
    key: &'static str,
    configure: impl FnOnce(RegistrationOptions<T>) -> RegistrationOptions<T>,
) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let opts = configure(RegistrationOptions::new());

    // Validate: local_only conflicts with serialization options.
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

    let mut registry = REGISTRY.write().expect("registry lock poisoned");
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
            // Custom codec deserializer.
            deserializers.insert(
                opts.version,
                Box::new(move |bytes: &[u8]| -> Result<Box<dyn ContextValue>, ContextError> {
                    decode(bytes)
                        .map(|v| Box::new(v) as Box<dyn ContextValue>)
                        .map_err(ContextError::DeserializationFailed)
                }),
            );
        } else {
            // Default bincode deserializer.
            deserializers.insert(
                opts.version,
                Box::new(|bytes: &[u8]| -> Result<Box<dyn ContextValue>, ContextError> {
                    bincode::deserialize::<T>(bytes)
                        .map(|v| Box::new(v) as Box<dyn ContextValue>)
                        .map_err(|e| ContextError::DeserializationFailed(e.to_string()))
                }),
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
        },
    );
    Ok(())
}

/// Panicking convenience wrapper for [`try_register`].
pub fn register<T>(key: &'static str)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register::<T>(key).expect("dcontext::register failed");
}

/// Panicking convenience wrapper for [`try_register_with`].
pub fn register_with<T>(
    key: &'static str,
    configure: impl FnOnce(RegistrationOptions<T>) -> RegistrationOptions<T>,
)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register_with::<T>(key, configure).expect("dcontext::register_with failed");
}

/// Register a local-only context type. The type does NOT need to implement
/// `Serialize`/`DeserializeOwned`. Local-only entries propagate via
/// `snapshot()`/`attach()` but are excluded from serialization.
///
/// Use this instead of `register_with(..., |o| o.local_only())` when `T`
/// does not implement `Serialize`.
pub fn try_register_local<T>(key: &'static str) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + 'static,
{
    let mut registry = REGISTRY.write().expect("registry lock poisoned");
    let tid = TypeId::of::<T>();

    if let Some(existing) = registry.get(key) {
        if existing.type_id == tid {
            return Ok(());
        }
        return Err(ContextError::AlreadyRegistered(key.to_string()));
    }

    registry.insert(
        key,
        Registration {
            key,
            type_id: tid,
            key_version: 0,
            deserializers: HashMap::new(),
            type_name: std::any::type_name::<T>(),
            local_only: true,
            serialize_fn: None,
        },
    );
    Ok(())
}

/// Panicking convenience wrapper for [`try_register_local`].
pub fn register_local<T>(key: &'static str)
where
    T: Clone + Default + Send + Sync + 'static,
{
    try_register_local::<T>(key).expect("dcontext::register_local failed");
}

/// Register a migration deserializer for an older wire version. The key must
/// already be registered with the current type `TCurrent`. When bytes arrive
/// with `wire_version == old_version`, they are deserialized as `TOld` and
/// then converted to `TCurrent` via the provided `migrate` function.
///
/// This enables rolling upgrades where old and new nodes coexist:
/// ```rust,ignore
/// dcontext::register_with::<TraceContextV2>("trace", |o| o.version(2));
/// dcontext::register_migration::<TraceContextV1, TraceContextV2>(
///     "trace", 1, |v1| TraceContextV2 { trace_id: v1.trace_id, span_id: String::new() }
/// );
/// ```
pub fn try_register_migration<TOld, TCurrent>(
    key: &'static str,
    old_version: u32,
    migrate: impl Fn(TOld) -> TCurrent + Send + Sync + 'static,
) -> Result<(), ContextError>
where
    TOld: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    TCurrent: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let mut registry = REGISTRY.write().expect("registry lock poisoned");

    let reg = registry.get_mut(key).ok_or_else(|| {
        ContextError::NotRegistered(key.to_string())
    })?;

    if reg.type_id != TypeId::of::<TCurrent>() {
        return Err(ContextError::TypeMismatch(
            key.to_string(),
            reg.type_name.to_string(),
            std::any::type_name::<TCurrent>().to_string(),
        ));
    }

    if reg.local_only {
        return Err(ContextError::SerializationFailed(format!(
            "cannot register migration for local-only key '{}'", key
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
        Box::new(move |bytes: &[u8]| -> Result<Box<dyn ContextValue>, ContextError> {
            let old_val = bincode::deserialize::<TOld>(bytes)
                .map_err(|e| ContextError::DeserializationFailed(e.to_string()))?;
            let current_val = migrate(old_val);
            Ok(Box::new(current_val) as Box<dyn ContextValue>)
        }),
    );

    Ok(())
}

/// Panicking convenience wrapper for [`try_register_migration`].
pub fn register_migration<TOld, TCurrent>(
    key: &'static str,
    old_version: u32,
    migrate: impl Fn(TOld) -> TCurrent + Send + Sync + 'static,
)
where
    TOld: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    TCurrent: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register_migration::<TOld, TCurrent>(key, old_version, migrate)
        .expect("dcontext::register_migration failed");
}

/// Check if a key is registered as local-only.
pub(crate) fn is_local(key: &str) -> bool {
    REGISTRY
        .read()
        .expect("registry lock poisoned")
        .get(key)
        .map(|r| r.local_only)
        .unwrap_or(false)
}

/// Look up a registration by key. Returns None if not registered.
pub(crate) fn with_registration<R>(
    key: &str,
    f: impl FnOnce(&Registration) -> R,
) -> Option<R> {
    let registry = REGISTRY.read().expect("registry lock poisoned");
    registry.get(key).map(f)
}

/// Info needed by `serialize_context`, fetched in a single lookup.
pub(crate) struct SerializationInfo {
    pub local_only: bool,
    pub key_version: u32,
    pub serialize_fn: Option<SerializeFn>,
}

/// Single-lookup extraction of everything `serialize_context` needs.
/// The lock is released before the caller invokes any closures.
pub(crate) fn get_serialization_info(key: &str) -> Option<SerializationInfo> {
    let registry = REGISTRY.read().expect("registry lock poisoned");
    registry.get(key).map(|r| SerializationInfo {
        local_only: r.local_only,
        key_version: r.key_version,
        serialize_fn: r.serialize_fn.clone(),
    })
}

/// Check if a key is registered.
pub(crate) fn is_registered(key: &str) -> bool {
    REGISTRY.read().expect("registry lock poisoned").contains_key(key)
}

/// Get the TypeId for a registered key.
pub(crate) fn type_id_for(key: &str) -> Option<TypeId> {
    REGISTRY.read().expect("registry lock poisoned").get(key).map(|r| r.type_id)
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
}
