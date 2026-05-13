use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::ContextError;
use crate::registry::Registry;
use crate::snapshot::ContextSnapshot;
use crate::value::ContextValue;

const WIRE_VERSION: u32 = 2;

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

/// Type alias for deserialized entry result.
type DeserializedEntry = (&'static str, Box<dyn ContextValue>);

/// Serialize a snapshot into wire-format bytes.
pub(crate) fn serialize_from(
    registry: &Registry<'_>,
    values: HashMap<&'static str, Arc<dyn ContextValue>>,
    scope_chain_val: Vec<String>,
) -> Result<Vec<u8>, ContextError> {
    let mut entries = Vec::new();

    for (key, val) in &values {
        let info = registry.get_serialization_info(key);

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

/// Deserialize bytes into a standalone snapshot.
pub(crate) fn deserialize_to_snapshot(
    registry: &Registry<'_>,
    bytes: &[u8],
) -> Result<ContextSnapshot, ContextError> {
    let (entries, scope_chain) = deserialize_wire(bytes)?;
    let mut values = HashMap::new();

    for entry in &entries {
        if let Some((static_key, val)) = deserialize_entry(registry, entry)? {
            values.insert(static_key, Arc::from(val));
        }
    }

    Ok(ContextSnapshot {
        values: Arc::new(values),
        scope_chain,
    })
}

fn deserialize_entry(
    registry: &Registry<'_>,
    entry: &WireEntry,
) -> Result<Option<DeserializedEntry>, ContextError> {
    let key_str = entry.key.as_str();
    let restored = registry.with_registration(key_str, |reg| {
        if reg.deserializers.is_empty() {
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
        Some(Some((static_key, Ok(val)))) => Ok(Some((static_key, val))),
        Some(Some((_, Err(e)))) => Err(e),
        Some(None) | None => Ok(None),
    }
}

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

/// Construct wire-format bytes with a single entry.
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
    bincode::serialize(&wire).unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod test_helpers {
    pub use super::{make_wire_bytes, make_wire_bytes_v};
}
