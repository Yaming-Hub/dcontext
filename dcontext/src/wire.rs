use serde::{Deserialize, Serialize};

use crate::error::ContextError;
use crate::registry;
use crate::scope::ScopeGuard;
use crate::storage;

const WIRE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct WireContext {
    version: u32,
    entries: Vec<WireEntry>,
}

#[derive(Serialize, Deserialize)]
struct WireEntry {
    key: String,
    key_version: u32,
    value: Vec<u8>,
}

/// Serialize the current context (all scopes merged) into bytes.
pub fn serialize_context() -> Result<Vec<u8>, ContextError> {
    let values = storage::collect_values();
    let mut entries = Vec::new();

    for (key, val) in &values {
        let value_bytes = val.serialize_value()?;
        let key_version = registry::with_registration(key, |r| r.key_version).unwrap_or(1);
        entries.push(WireEntry {
            key: key.to_string(),
            key_version,
            value: value_bytes,
        });
    }

    let wire = WireContext {
        version: WIRE_VERSION,
        entries,
    };

    bincode::serialize(&wire).map_err(|e| ContextError::SerializationFailed(e.to_string()))
}

/// Serialize the current context into a base64-encoded string.
#[cfg(feature = "base64")]
pub fn serialize_context_string() -> Result<String, ContextError> {
    use base64::Engine;
    let bytes = serialize_context()?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

/// Restore context from bytes. Pushes a new scope with deserialized values.
pub fn deserialize_context(bytes: &[u8]) -> Result<ScopeGuard, ContextError> {
    let wire: WireContext =
        bincode::deserialize(bytes).map_err(|e| ContextError::DeserializationFailed(e.to_string()))?;

    if wire.version != WIRE_VERSION {
        return Err(ContextError::DeserializationFailed(format!(
            "unsupported wire version: {} (expected {})",
            wire.version, WIRE_VERSION
        )));
    }

    let guard = storage::enter_scope();

    for entry in &wire.entries {
        let key_str = entry.key.as_str();
        // Only restore keys we have registered.
        let restored = registry::with_registration(key_str, |reg| {
            (reg.deserialize_fn)(&entry.value, entry.key_version)
        });

        match restored {
            Some(Ok(val)) => {
                // We need the &'static str from the registry.
                if let Some(static_key) = registry::with_registration(key_str, |r| r.key) {
                    storage::set_value(static_key, val);
                }
            }
            Some(Err(e)) => return Err(e),
            None => {
                // Unknown key — silently skip.
            }
        }
    }

    Ok(guard)
}

/// Restore context from a base64-encoded string.
#[cfg(feature = "base64")]
pub fn deserialize_context_string(encoded: &str) -> Result<ScopeGuard, ContextError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| ContextError::DeserializationFailed(e.to_string()))?;
    deserialize_context(&bytes)
}
