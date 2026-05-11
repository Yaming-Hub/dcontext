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
}

impl Default for ContextSnapshot {
    fn default() -> Self {
        Self::empty()
    }
}
