use std::any::TypeId;
use std::collections::HashMap;
use std::sync::RwLock;

use crate::error::ContextError;
use crate::value::ContextValue;

/// Type alias for versioned deserializer functions.
type DeserializeFn = Box<dyn Fn(&[u8]) -> Result<Box<dyn ContextValue>, ContextError> + Send + Sync>;

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
}

static REGISTRY: std::sync::LazyLock<RwLock<HashMap<&'static str, Registration>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

/// Register a context type. Idempotent if same key+type; errors if key
/// registered with a different type.
pub fn try_register<T>(key: &'static str) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register_versioned::<T>(key, 1)
}

/// Register with an explicit key version for wire format evolution.
pub fn try_register_versioned<T>(key: &'static str, version: u32) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let mut registry = REGISTRY.write().unwrap();
    let tid = TypeId::of::<T>();

    if let Some(existing) = registry.get(key) {
        if existing.type_id == tid {
            return Ok(()); // idempotent
        }
        return Err(ContextError::AlreadyRegistered(key.to_string()));
    }

    let mut deserializers: HashMap<u32, DeserializeFn> = HashMap::new();
    deserializers.insert(
        version,
        Box::new(|bytes: &[u8]| -> Result<Box<dyn ContextValue>, ContextError> {
            bincode::deserialize::<T>(bytes)
                .map(|v| Box::new(v) as Box<dyn ContextValue>)
                .map_err(|e| ContextError::DeserializationFailed(e.to_string()))
        }),
    );

    registry.insert(
        key,
        Registration {
            key,
            type_id: tid,
            key_version: version,
            deserializers,
            type_name: std::any::type_name::<T>(),
            local_only: false,
        },
    );
    Ok(())
}

/// Panicking convenience wrapper.
pub fn register<T>(key: &'static str)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register::<T>(key).expect("dcontext::register failed");
}

/// Panicking convenience wrapper for versioned registration.
pub fn register_versioned<T>(key: &'static str, version: u32)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_register_versioned::<T>(key, version).expect("dcontext::register_versioned failed");
}

/// Register a migration deserializer for an older wire version. The key must
/// already be registered with the current type `TCurrent`. When bytes arrive
/// with `wire_version == old_version`, they are deserialized as `TOld` and
/// then converted to `TCurrent` via the provided `migrate` function.
///
/// This enables rolling upgrades where old and new nodes coexist:
/// ```rust,ignore
/// // Current version
/// dcontext::register_versioned::<TraceContextV2>("trace", 2);
/// // Accept old V1 bytes and convert to V2
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
    let mut registry = REGISTRY.write().unwrap();

    let reg = registry.get_mut(key).ok_or_else(|| {
        ContextError::NotRegistered(key.to_string())
    })?;

    // Verify the current type matches TCurrent.
    if reg.type_id != TypeId::of::<TCurrent>() {
        return Err(ContextError::TypeMismatch(
            key.to_string(),
            reg.type_name.to_string(),
            std::any::type_name::<TCurrent>().to_string(),
        ));
    }

    // Prevent overwriting the current version's native deserializer.
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

/// Panicking convenience wrapper for migration registration.
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

/// Register a local-only context type. Local-only entries are propagated via
/// `snapshot()`/`attach()` within the same process but are silently excluded
/// from `serialize_context()`. The type does NOT need to implement
/// `Serialize`/`DeserializeOwned`.
pub fn try_register_local<T>(key: &'static str) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + 'static,
{
    let mut registry = REGISTRY.write().unwrap();
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
        },
    );
    Ok(())
}

/// Panicking convenience wrapper for local-only registration.
pub fn register_local<T>(key: &'static str)
where
    T: Clone + Default + Send + Sync + 'static,
{
    try_register_local::<T>(key).expect("dcontext::register_local failed");
}

/// Check if a key is registered as local-only.
pub(crate) fn is_local(key: &str) -> bool {
    REGISTRY
        .read()
        .unwrap()
        .get(key)
        .map(|r| r.local_only)
        .unwrap_or(false)
}

/// Look up a registration by key. Returns None if not registered.
pub(crate) fn with_registration<R>(
    key: &str,
    f: impl FnOnce(&Registration) -> R,
) -> Option<R> {
    let registry = REGISTRY.read().unwrap();
    registry.get(key).map(f)
}

/// Check if a key is registered.
pub(crate) fn is_registered(key: &str) -> bool {
    REGISTRY.read().unwrap().contains_key(key)
}

/// Get the TypeId for a registered key.
pub(crate) fn type_id_for(key: &str) -> Option<TypeId> {
    REGISTRY.read().unwrap().get(key).map(|r| r.type_id)
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
