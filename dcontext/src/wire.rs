use serde::{Deserialize, Serialize};

use crate::error::ContextError;
use crate::registry;
use crate::scope::ScopeGuard;
use crate::storage;

const WIRE_VERSION: u32 = 2;

/// Wire format v2: includes scope chain alongside entries.
#[derive(Serialize, Deserialize)]
struct WireContext {
    version: u32,
    entries: Vec<WireEntry>,
    /// The scope chain at serialization time. Empty for v1 messages.
    scope_chain: Vec<String>,
}

/// Wire format v1: entries only, no scope chain.
#[derive(Serialize, Deserialize)]
struct WireContextV1 {
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
        // Single registry lookup: fetch local_only, key_version, and
        // serialize_fn (Arc-cloned) so no work runs under the lock.
        let info = registry::get_serialization_info(key);

        // Skip local-only entries — they don't cross process boundaries.
        // Check both the registry flag and the value trait method (the
        // latter covers values stored via set_context_local).
        let is_local = val.is_local()
            || info.as_ref().map_or(false, |i| i.local_only);
        if is_local {
            continue;
        }

        // Use custom serialize_fn if registered, otherwise default (bincode via ContextValue).
        let value_bytes = match info.as_ref().and_then(|i| i.serialize_fn.as_ref()) {
            Some(custom_ser) => custom_ser(val.as_ref())?,
            None => val.serialize_value()?,
        };
        let key_version = info.as_ref().map_or(1, |i| i.key_version);
        entries.push(WireEntry {
            key: key.to_string(),
            key_version,
            value: value_bytes,
        });
    }

    let wire = WireContext {
        version: WIRE_VERSION,
        entries,
        scope_chain: storage::collect_scope_chain(),
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
    // Parse version prefix to decide format.
    let (entries, scope_chain) = deserialize_wire(bytes)?;

    let guard = storage::enter_scope();

    // Restore remote scope chain.
    if !scope_chain.is_empty() {
        storage::set_remote_chain(scope_chain);
    }

    for entry in &entries {
        let key_str = entry.key.as_str();
        // Single registry lookup: find the right versioned deserializer.
        let restored = registry::with_registration(key_str, |reg| {
            if reg.local_only || reg.deserializers.is_empty() {
                return None; // local-only or no deserializers — skip
            }
            match reg.deserializers.get(&entry.key_version) {
                Some(deser_fn) => {
                    let val = deser_fn(&entry.value);
                    Some((reg.key, val))
                }
                None => Some((reg.key, Err(ContextError::DeserializationFailed(format!(
                    "no deserializer for key '{}' wire version {} (registered versions: {:?})",
                    key_str,
                    entry.key_version,
                    reg.deserializers.keys().collect::<Vec<_>>()
                )))))
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

/// Deserialize wire bytes, handling both v1 and v2 formats.
fn deserialize_wire(bytes: &[u8]) -> Result<(Vec<WireEntry>, Vec<String>), ContextError> {
    // Try v2 first (current version).
    if let Ok(wire) = bincode::deserialize::<WireContext>(bytes) {
        if wire.version == 2 {
            return Ok((wire.entries, wire.scope_chain));
        }
        // Future versions (v3+) that happen to deserialize as v2 struct:
        // reject with a clear error rather than falling through to v1.
        if wire.version > 2 {
            return Err(ContextError::DeserializationFailed(format!(
                "unsupported wire version: {} (this library supports versions 1 and 2)",
                wire.version
            )));
        }
    }

    // Fall back to v1.
    let wire: WireContextV1 = bincode::deserialize(bytes)
        .map_err(|e| ContextError::DeserializationFailed(e.to_string()))?;

    if wire.version != 1 {
        return Err(ContextError::DeserializationFailed(format!(
            "unsupported wire version: {} (expected 1 or 2)",
            wire.version
        )));
    }

    Ok((wire.entries, Vec::new()))
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

/// Construct wire-format bytes with a single entry. This is a helper for
/// testing version migration — it lets you create wire bytes as if they came
/// from a sender running an older schema version.
///
/// In production, wire bytes come from `serialize_context()` on the sender.
/// This function is useful in tests and samples to simulate cross-version
/// scenarios within a single process.
///
/// The `wire_version` parameter controls which wire format version to emit.
/// Use `1` to simulate a pre-scope-chain sender, or `2` (current) for full format.
pub fn make_wire_bytes(key: &str, key_version: u32, value_bytes: &[u8]) -> Vec<u8> {
    make_wire_bytes_v(2, key, key_version, value_bytes)
}

/// Like [`make_wire_bytes`] but allows specifying the wire format version.
pub fn make_wire_bytes_v(wire_version: u32, key: &str, key_version: u32, value_bytes: &[u8]) -> Vec<u8> {
    if wire_version == 1 {
        // V1 format: no scope_chain field.
        let wire = WireContextV1 {
            version: 1,
            entries: vec![WireEntry {
                key: key.to_string(),
                key_version,
                value: value_bytes.to_vec(),
            }],
        };
        bincode::serialize(&wire)
            .expect("dcontext::make_wire_bytes_v: bincode serialization should not fail")
    } else {
        let wire = WireContext {
            version: wire_version,
            entries: vec![WireEntry {
                key: key.to_string(),
                key_version,
                value: value_bytes.to_vec(),
            }],
            scope_chain: Vec::new(),
        };
        bincode::serialize(&wire)
            .expect("dcontext::make_wire_bytes_v: bincode serialization should not fail")
    }
}

/// Test helpers (re-exports make_wire_bytes for internal tests).
#[cfg(test)]
pub(crate) mod test_helpers {
    pub use super::{make_wire_bytes, make_wire_bytes_v};
}
