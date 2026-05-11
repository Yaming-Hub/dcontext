use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::ContextError;
use crate::registry;
use crate::scope::ScopeGuard;
use crate::sync_ctx as storage;
use crate::value::ContextValue;

const WIRE_VERSION: u32 = 2;

/// Wire format: includes scope chain alongside entries.
/// For v1 messages (no scope chain), `scope_chain` defaults to empty.
#[derive(Serialize, Deserialize)]
struct WireContext {
    version: u32,
    entries: Vec<WireEntry>,
    #[serde(default)]
    scope_chain: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct WireEntry {
    key: String,
    key_version: u32,
    value: Vec<u8>,
}

/// Serialize context from pre-collected values and scope chain.
///
/// This is the shared implementation used by both `sync_ctx::serialize_context`
/// and `async_ctx::serialize_context`.
pub(crate) fn serialize_from(
    values: HashMap<&'static str, Arc<dyn ContextValue>>,
    scope_chain_val: Vec<String>,
) -> Result<Vec<u8>, ContextError> {
    let mut entries = Vec::new();

    for (key, val) in &values {
        // Single registry lookup: fetch local_only, key_version, and
        // serialize_fn (Arc-cloned) so no work runs under the lock.
        let info = registry::get_serialization_info(key);

        // Skip local-only entries — they don't cross process boundaries.
        // Check both the registry flag and the value trait method (the
        // latter covers values stored via set_context_local).
        let is_local = val.is_local() || info.as_ref().is_some_and(|i| i.local_only);
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
        scope_chain: scope_chain_val,
    };

    let bytes =
        bincode::serialize(&wire).map_err(|e| ContextError::SerializationFailed(e.to_string()))?;

    crate::config::check_size(bytes.len())?;

    Ok(bytes)
}

/// Restore context from bytes into the specified store (sync or async).
///
/// Pushes a new scope, activates a scope barrier, and populates the scope
/// with deserialized values. This is the shared implementation used by both
/// `sync_ctx::deserialize_context` and `async_ctx::deserialize_context`.
pub(crate) fn deserialize_into(bytes: &[u8], use_async: bool) -> Result<ScopeGuard, ContextError> {
    let (entries, scope_chain) = deserialize_wire(bytes)?;

    let guard = if use_async {
        crate::async_ctx::push_scope("")
    } else {
        storage::enter_scope()
    };

    // Activate scope barrier — hides all parent scopes from lookups.
    if use_async {
        crate::async_ctx::set_scope_barrier();
    } else {
        storage::set_scope_barrier();
    }

    // Restore remote scope chain.
    if !scope_chain.is_empty() {
        if use_async {
            crate::async_ctx::set_remote_chain(scope_chain);
        } else {
            storage::set_remote_chain(scope_chain);
        }
    }

    for entry in &entries {
        let key_str = entry.key.as_str();
        let restored = registry::with_registration(key_str, |reg| {
            if reg.local_only || reg.deserializers.is_empty() {
                return None;
            }
            match reg.deserializers.get(&entry.key_version) {
                Some(deser_fn) => {
                    let val = deser_fn(&entry.value);
                    Some((reg.key, val))
                }
                None => Some((
                    reg.key,
                    Err(ContextError::DeserializationFailed(format!(
                        "no deserializer for key '{}' wire version {} (registered versions: {:?})",
                        key_str,
                        entry.key_version,
                        reg.deserializers.keys().collect::<Vec<_>>()
                    ))),
                )),
            }
        });

        match restored {
            Some(Some((static_key, Ok(val)))) => {
                if use_async {
                    crate::async_ctx::set_raw_value(static_key, std::sync::Arc::from(val));
                } else {
                    storage::set_value(static_key, std::sync::Arc::from(val));
                }
            }
            Some(Some((_, Err(e)))) => return Err(e),
            Some(None) | None => {}
        }
    }

    Ok(guard)
}

/// Deserialize wire bytes, handling both v1 and v2 formats.
fn deserialize_wire(bytes: &[u8]) -> Result<(Vec<WireEntry>, Vec<String>), ContextError> {
    let wire: WireContext = bincode::deserialize(bytes)
        .map_err(|e| ContextError::DeserializationFailed(e.to_string()))?;

    match wire.version {
        1 | 2 => Ok((wire.entries, wire.scope_chain)),
        v => Err(ContextError::DeserializationFailed(format!(
            "unsupported wire version: {} (this library supports versions 1 and 2)",
            v
        ))),
    }
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
pub fn make_wire_bytes_v(
    wire_version: u32,
    key: &str,
    key_version: u32,
    value_bytes: &[u8],
) -> Vec<u8> {
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

/// Test helpers (re-exports make_wire_bytes for internal tests).
#[cfg(test)]
pub(crate) mod test_helpers {
    pub use super::{make_wire_bytes, make_wire_bytes_v};
}
