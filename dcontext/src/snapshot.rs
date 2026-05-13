use std::collections::HashMap;
use std::sync::Arc;

use crate::value::ContextValue;

/// An immutable snapshot of the current context. Clone + Send + Sync.
#[derive(Clone)]
pub struct ContextSnapshot {
    pub(crate) values: Arc<HashMap<&'static str, Arc<dyn ContextValue>>>,
    /// The scope chain at the time the snapshot was taken.
    pub(crate) scope_chain: Vec<String>,
}

impl ContextSnapshot {
    /// Create an empty snapshot.
    pub fn empty() -> Self {
        Self {
            values: Arc::new(HashMap::new()),
            scope_chain: Vec::new(),
        }
    }

    /// Return the scope chain captured in this snapshot.
    pub fn scope_chain(&self) -> &[String] {
        &self.scope_chain
    }

    /// Serialize this snapshot to wire-format bytes.
    pub fn serialize(&self) -> Result<Vec<u8>, crate::error::ContextError> {
        crate::registry::with_global_registry(|registry| {
            crate::wire::serialize_from(
                registry,
                self.values
                    .iter()
                    .map(|(k, v)| (*k, Arc::clone(v)))
                    .collect(),
                self.scope_chain.clone(),
            )
        })
    }

    /// Deserialize wire-format bytes into a snapshot.
    pub fn deserialize(bytes: &[u8]) -> Result<Self, crate::error::ContextError> {
        crate::registry::with_global_registry(|registry| {
            crate::wire::deserialize_to_snapshot(registry, bytes)
        })
    }
}

impl Default for ContextSnapshot {
    fn default() -> Self {
        Self::empty()
    }
}
