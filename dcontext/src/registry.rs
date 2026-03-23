use std::any::TypeId;
use std::collections::HashMap;
use std::sync::RwLock;

use crate::error::ContextError;
use crate::value::ContextValue;

/// Metadata stored for each registered context key.
pub(crate) struct Registration {
    pub key: &'static str,
    pub type_id: TypeId,
    pub key_version: u32,
    pub deserialize_fn: Option<Box<dyn Fn(&[u8], u32) -> Result<Box<dyn ContextValue>, ContextError> + Send + Sync>>,
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

    registry.insert(
        key,
        Registration {
            key,
            type_id: tid,
            key_version: version,
            deserialize_fn: Some({
                let registered_version = version;
                Box::new(move |bytes: &[u8], wire_version: u32| -> Result<Box<dyn ContextValue>, ContextError> {
                    if wire_version != registered_version {
                        return Err(ContextError::DeserializationFailed(format!(
                            "key version mismatch: wire={}, registered={}",
                            wire_version, registered_version
                        )));
                    }
                    bincode::deserialize::<T>(bytes)
                        .map(|v| Box::new(v) as Box<dyn ContextValue>)
                        .map_err(|e| ContextError::DeserializationFailed(e.to_string()))
                })
            }),
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
            deserialize_fn: None,
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
