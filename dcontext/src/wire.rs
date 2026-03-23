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
        // Skip local-only entries — they don't cross process boundaries.
        if val.is_local() {
            continue;
        }
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

    let bytes = bincode::serialize(&wire)
        .map_err(|e| ContextError::SerializationFailed(e.to_string()))?;

    crate::config::check_size(bytes.len())?;

    Ok(bytes)
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
        // Single registry lookup: get both deserialize_fn and static key.
        let restored = registry::with_registration(key_str, |reg| {
            match &reg.deserialize_fn {
                Some(deser_fn) => {
                    let val = deser_fn(&entry.value, entry.key_version);
                    Some((reg.key, val))
                }
                None => None, // local-only key — skip
            }
        });

        match restored {
            Some(Some((static_key, Ok(val)))) => {
                storage::set_value(static_key, val);
            }
            Some(Some((_, Err(e)))) => return Err(e),
            Some(None) | None => {
                // Unknown key or local-only — silently skip.
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
