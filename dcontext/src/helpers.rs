use std::cell::Cell;
use std::sync::Arc;

use crate::snapshot::{self, ContextSnapshot};
use crate::store::ContextStore;
use crate::async_ctx::TASK_CONTEXT;

// ── Deprecated compatibility shims ────────────────────────────

/// Opaque handle for the deprecated `fork()` / `with_fork()` API.
///
/// **Deprecated**: Use the spawn helpers in [`crate::inheritance`] instead.
pub struct ForkHandle(Option<ContextStore>);

impl Clone for ForkHandle {
    fn clone(&self) -> Self {
        // Clone the inner store directly — both handles share the same
        // frozen parent chain, so reads see the same values.
        Self(self.0.as_ref().map(|s| ContextStore {
            scope_chain: s.scope_chain.clone(),
            current_values: s.current_values.iter().map(|(&k, v)| (k, Arc::clone(v))).collect(),
            current_name: s.current_name.clone(),
            depth: s.depth,
            remote_chain: Arc::clone(&s.remote_chain),
            remote_chain_base_depth: s.remote_chain_base_depth,
            frozen_parent: s.frozen_parent.clone(),
        }))
    }
}

/// Create a lightweight fork handle from the current context.
///
/// **Deprecated**: Use [`spawn_with_sync_context`](crate::spawn_with_sync_context)
/// or [`spawn_with_async_context`](crate::spawn_with_async_context).
#[deprecated(note = "Use spawn_with_sync_context or spawn_with_async_context")]
pub fn fork() -> ForkHandle {
    ForkHandle(crate::sync_ctx::fork())
}

/// Run an async future with a forked context.
///
/// **Deprecated**: Use the spawn helpers instead.
#[deprecated(note = "Use spawn_with_async_context or spawn_with_sync_context")]
pub async fn with_fork<F>(handle: ForkHandle, future: F) -> F::Output
where
    F: std::future::Future,
{
    let store = handle.0.unwrap_or_else(ContextStore::new);
    TASK_CONTEXT.scope(Cell::new(Some(store)), future).await
}

/// Spawn a Tokio task with forked sync context.
///
/// **Deprecated**: Use [`spawn_with_sync_context`](crate::spawn_with_sync_context).
#[deprecated(note = "Use spawn_with_sync_context")]
pub fn spawn_with_fork_async<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    crate::inheritance::spawn_with_sync_context(
        crate::inheritance::ContextInheritance::Fork,
        future,
    )
}

/// Spawn a std::thread that inherits the current context via snapshot.
///
/// **Deprecated**: Use `snapshot()` + `attach()` in the spawned thread.
#[deprecated(note = "Use snapshot() + attach() in the spawned thread")]
pub fn spawn_with_context<F, T>(name: &str, f: F) -> std::io::Result<std::thread::JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let snap = snapshot::snapshot();
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let _guard = snapshot::attach(snap);
            f()
        })
}

/// **Deprecated**: Use the spawn helpers instead.
#[deprecated(note = "Use spawn_with_async_context or spawn_with_sync_context with ContextInheritance::Snapshot")]
pub async fn with_context<F, T>(snap: ContextSnapshot, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let chain = snap.scope_chain.clone();
    let values = snap
        .values
        .iter()
        .map(|(k, v)| (*k, Arc::clone(v)))
        .collect();
    let store = ContextStore::from_values_with_chain(values, chain);
    TASK_CONTEXT.scope(Cell::new(Some(store)), f).await
}

/// **Deprecated**: Use [`spawn_with_async_context`](crate::spawn_with_async_context).
#[deprecated(note = "Use spawn_with_async_context(ContextInheritance::Snapshot, future)")]
pub fn spawn_with_context_async<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    crate::inheritance::spawn_with_async_context(
        crate::inheritance::ContextInheritance::Snapshot,
        future,
    )
}
