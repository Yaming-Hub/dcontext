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
    pub default_fn: fn() -> Box<dyn ContextValue>,
    pub deserialize_fn: fn(&[u8], u32) -> Result<Box<dyn ContextValue>, ContextError>,
    pub type_name: &'static str,
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
            default_fn: || Box::new(T::default()),
            deserialize_fn: |bytes, _version| {
                bincode::deserialize::<T>(bytes)
                    .map(|v| Box::new(v) as Box<dyn ContextValue>)
                    .map_err(|e| ContextError::DeserializationFailed(e.to_string()))
            },
            type_name: std::any::type_name::<T>(),
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

/// Look up a registration by key. Returns None if not registered.
pub(crate) fn with_registration<R>(
    key: &str,
    f: impl FnOnce(&Registration) -> R,
) -> Option<R> {
    let registry = REGISTRY.read().unwrap();
    registry.get(key).map(f)
}

/// Iterate all registrations (used for serialization).
pub(crate) fn for_each_registration(mut f: impl FnMut(&Registration)) {
    let registry = REGISTRY.read().unwrap();
    for reg in registry.values() {
        f(reg);
    }
}

/// Check if a key is registered.
pub(crate) fn is_registered(key: &str) -> bool {
    REGISTRY.read().unwrap().contains_key(key)
}

/// Get the TypeId for a registered key.
pub(crate) fn type_id_for(key: &str) -> Option<TypeId> {
    REGISTRY.read().unwrap().get(key).map(|r| r.type_id)
}

/// Clear all registrations (for testing only).
#[cfg(test)]
pub(crate) fn clear_registry() {
    REGISTRY.write().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Default, Debug, Serialize, Deserialize)]
    struct TestVal(String);

    #[derive(Clone, Default, Debug, Serialize, Deserialize)]
    struct OtherVal(u64);

    #[test]
    fn register_and_lookup() {
        clear_registry();
        try_register::<TestVal>("test_val").unwrap();
        assert!(is_registered("test_val"));
        assert!(!is_registered("missing"));
    }

    #[test]
    fn idempotent_registration() {
        clear_registry();
        try_register::<TestVal>("idem_key").unwrap();
        // Same key+type is fine
        try_register::<TestVal>("idem_key").unwrap();
    }

    #[test]
    fn conflicting_registration() {
        clear_registry();
        try_register::<TestVal>("conflict_key").unwrap();
        let err = try_register::<OtherVal>("conflict_key").unwrap_err();
        assert!(matches!(err, ContextError::AlreadyRegistered(_)));
    }
}
