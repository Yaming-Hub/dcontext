use std::collections::HashMap;
use std::sync::Arc;

use crate::scope::ScopeGuard;
use crate::storage;
use crate::value::ContextValue;

/// An immutable snapshot of the current context. Clone + Send + Sync.
#[derive(Clone)]
pub struct ContextSnapshot {
    pub(crate) values: Arc<HashMap<&'static str, Box<dyn ContextValue>>>,
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

/// Capture a snapshot of the current effective context.
pub fn snapshot() -> ContextSnapshot {
    let values = storage::collect_values();
    let scope_chain = storage::collect_scope_chain();
    ContextSnapshot {
        values: Arc::new(values),
        scope_chain,
    }
}

/// Restore a snapshot by pushing a new scope with its values.
pub fn attach(snap: ContextSnapshot) -> ScopeGuard {
    let guard = storage::enter_scope();
    // Restore the scope chain from the snapshot as the remote prefix.
    if !snap.scope_chain.is_empty() {
        storage::set_remote_chain(snap.scope_chain.clone());
    }
    // Clone each value from the snapshot into the new scope.
    for (key, val) in snap.values.iter() {
        storage::set_value(key, val.clone_boxed());
    }
    guard
}

/// Wrap a FnOnce closure so context is captured now and restored when called.
pub fn wrap_with_context<F, T>(f: F) -> impl FnOnce() -> T + Send
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let snap = snapshot();
    move || {
        let _guard = attach(snap);
        f()
    }
}

/// Wrap an Fn closure so context is captured now and restored on each call.
pub fn wrap_with_context_fn<F, T>(f: F) -> impl Fn() -> T + Send + Sync
where
    F: Fn() -> T + Send + Sync + 'static,
    T: Send + 'static,
{
    let snap = snapshot();
    move || {
        let _guard = attach(snap.clone());
        f()
    }
}
