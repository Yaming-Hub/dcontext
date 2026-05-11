use std::collections::HashMap;
use std::sync::Arc;

use crate::scope::ScopeGuard;
use crate::sync_ctx as storage;
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

/// Capture a snapshot of the current effective context.
/// Dispatches to task-local if available, else thread-local.
///
/// **Prefer** [`async_ctx::snapshot()`](crate::async_ctx::snapshot) or
/// [`sync_ctx::snapshot()`](crate::sync_ctx::snapshot) for explicit control.
pub fn snapshot() -> ContextSnapshot {
    if crate::async_ctx::current_depth().is_some() {
        return crate::async_ctx::snapshot();
    }
    crate::sync_ctx::snapshot()
}

/// Restore a snapshot by pushing a new scope with its values.
/// Dispatches to task-local if available, else thread-local.
pub fn attach(snap: ContextSnapshot) -> ScopeGuard {
    let use_async = crate::async_ctx::current_depth().is_some();
    let guard = if use_async {
        crate::async_ctx::push_scope("")
    } else {
        storage::enter_scope()
    };
    // Restore the scope chain from the snapshot as the remote prefix.
    if !snap.scope_chain.is_empty() && !use_async {
        storage::set_remote_chain(snap.scope_chain.clone());
    }
    // Clone each Arc value from the snapshot into the new scope.
    for (key, val) in snap.values.iter() {
        if use_async {
            crate::async_ctx::set_raw_value(key, Arc::clone(val));
        } else {
            storage::set_value(key, Arc::clone(val));
        }
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
