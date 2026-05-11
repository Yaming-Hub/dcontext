//! Async context module — operates exclusively on tokio task-local storage.
//!
//! All functions in this module access the **task-local** context store.
//! They no-op (or return empty results) if called outside an async task.
//!
//! This module is the recommended way to manage context in async code.
//! It never touches thread-local storage, eliminating the depth-mismatch
//! leak that occurs when `DcontextLayer` and application code share the
//! same thread-local store.

use std::cell::Cell;
use std::sync::Arc;

use crate::scope::ScopeGuard;
use crate::snapshot::ContextSnapshot;
use crate::storage::{ContextStore, TASK_CONTEXT};

/// Push a named scope onto the task-local store.
///
/// Returns a [`ScopeGuard`] that pops the scope on drop.
/// Returns a no-op guard if not called within an async task.
pub fn push_scope(name: &str) -> ScopeGuard {
    try_push_scope(name).unwrap_or_else(ScopeGuard::noop)
}

/// Try to push a named scope onto the task-local store.
///
/// Returns `Some(ScopeGuard)` if a task-local context is active,
/// or `None` if not called within an async task (no task-local context).
pub fn try_push_scope(name: &str) -> Option<ScopeGuard> {
    let name = name.to_string();
    with_task_store(|store| ScopeGuard::new(store.push_scope(Some(name))))
}

/// Pop the top scope from the task-local store.
///
/// This is typically done automatically by dropping the [`ScopeGuard`].
/// Use this only when manual scope management is needed.
pub fn pop_scope(expected_depth: usize) {
    let _garbage = with_task_store(|store| store.pop_scope(expected_depth));
}

/// Get the current scope chain from the task-local store.
///
/// Returns an empty `Vec` if not in an async task or if the store is busy.
pub fn scope_chain() -> Vec<String> {
    with_task_store(|store| store.scope_chain()).unwrap_or_default()
}

/// Set a context value in the task-local store.
///
/// Silently no-ops if not in an async task.
pub fn set_context<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    with_task_store(|store| {
        store.set_value(key, Arc::new(value));
    });
}

/// Get a context value from the task-local store.
///
/// Returns `None` if the key is not set or not in an async task.
pub fn get_context<T>(key: &str) -> Option<T>
where
    T: Clone + Send + Sync + 'static,
{
    with_task_store(|store| {
        store.get_value(key).and_then(|arc| {
            arc.as_any().downcast_ref::<T>().cloned()
        })
    })
    .flatten()
}

/// Take a snapshot of the current task-local context.
///
/// Used for bridging to sync context or propagating to child tasks.
/// Returns an empty snapshot if not in an async task.
pub fn snapshot() -> ContextSnapshot {
    with_task_store(|store| {
        let values = store.collect_values();
        let scope_chain = store.scope_chain();
        ContextSnapshot {
            values: Arc::new(values),
            scope_chain,
        }
    })
    .unwrap_or_default()
}

/// Run a future with a new task-local context initialized from a snapshot.
///
/// Used when spawning child tasks that should inherit parent context.
///
/// # Example
///
/// ```rust,ignore
/// let snap = dcontext::async_ctx::snapshot();
/// tokio::spawn(async move {
///     dcontext::async_ctx::with_context(snap, async {
///         // child task has parent's context
///     }).await;
/// });
/// ```
pub async fn with_context<F>(snapshot: ContextSnapshot, fut: F) -> F::Output
where
    F: std::future::Future,
{
    let chain = snapshot.scope_chain.clone();
    let values = snapshot
        .values
        .iter()
        .map(|(k, v)| (*k, Arc::clone(v)))
        .collect();
    let store = ContextStore::from_values_with_chain(values, chain);
    TASK_CONTEXT.scope(Cell::new(Some(store)), fut).await
}

/// Run a future with a scoped context (push before, pop after).
///
/// This is the async-safe replacement for `scope_async` / `named_scope_async`.
/// Always operates on task-local storage.
///
/// # Example
///
/// ```rust,ignore
/// dcontext::async_ctx::scope("my_operation", async {
///     // context changes here are reverted when the future completes
/// }).await;
/// ```
pub async fn scope<F>(name: &str, fut: F) -> F::Output
where
    F: std::future::Future,
{
    let name_owned = name.to_string();
    let depth = with_task_store(|store| store.push_scope(Some(name_owned)));

    match depth {
        None => fut.await,
        Some(depth) => {
            struct ScopeCleanup(usize);
            impl Drop for ScopeCleanup {
                fn drop(&mut self) {
                    super::async_ctx::pop_scope(self.0);
                }
            }
            let cleanup = ScopeCleanup(depth);
            let result = fut.await;
            std::mem::forget(cleanup);
            pop_scope(depth);
            result
        }
    }
}

// ── Internal helpers ───────────────────────────────────────────

/// Execute `f` with exclusive access to the task-local context store.
/// Returns `None` if not in an async task or if the store is busy.
fn with_task_store<R>(f: impl FnOnce(&mut ContextStore) -> R) -> Option<R> {
    let found = TASK_CONTEXT.try_with(|cell| {
        let mut store = cell.take()?;
        let r = f(&mut store);
        cell.set(Some(store));
        Some(r)
    });
    match found {
        Ok(Some(r)) => Some(r),
        _ => None,
    }
}
