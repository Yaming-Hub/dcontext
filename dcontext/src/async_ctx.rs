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
use crate::store::ContextStore;
use crate::value::ContextValue;

// ── Task-local storage ─────────────────────────────────────────

tokio::task_local! {
    /// Task-local context store. Each async task gets its own isolated store.
    /// Accessed via `async_ctx` module functions.
    pub(crate) static TASK_CONTEXT: Cell<Option<ContextStore>>;
}

// ── Scope management ───────────────────────────────────────────

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
    try_apply(|store| ScopeGuard::new_async(store.push_scope(Some(name))))
}

/// Pop the top scope from the task-local store.
///
/// This is typically done automatically by dropping the [`ScopeGuard`].
/// Use this only when manual scope management is needed.
pub fn pop_scope(expected_depth: usize) {
    let _garbage = try_apply(|store| store.pop_scope(expected_depth));
}

/// Activate a scope barrier that hides all parent scopes from lookups.
///
/// Used by `deserialize_context` so the restored remote context fully
/// replaces the visible values. The barrier is saved/restored by
/// push_scope/pop_scope, so dropping the scope guard clears it.
pub(crate) fn set_scope_barrier() {
    try_apply(|store| store.set_scope_barrier());
}

/// Returns `true` if a task-local context store is available.
///
/// This indicates the code is running inside an `async_ctx::with_context`
/// scope (i.e., a tokio task with an initialized task-local store).
pub fn is_active() -> bool {
    try_apply(|_| ()).is_some()
}

/// Peek at the current scope depth on the task-local store.
///
/// The depth uniquely identifies the active scope within the store.
/// Returns `None` if not in an async task or the store is busy.
pub fn current_depth() -> Option<usize> {
    try_apply(|store| store.depth)
}

/// Get the current scope chain from the task-local store.
///
/// Returns an empty `Vec` if not in an async task or if the store is busy.
pub fn scope_chain() -> Vec<String> {
    try_apply(|store| store.scope_chain()).unwrap_or_default()
}

// ── Value access (typed) ───────────────────────────────────────

/// Set a context value in the task-local store.
///
/// Silently no-ops if not in an async task.
pub fn set_context<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_apply(|store| {
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
    try_apply(|store| {
        store
            .get_value(key)
            .and_then(|arc| arc.as_any().downcast_ref::<T>().cloned())
    })
    .flatten()
}

/// Update a context value using a callback (read-modify-write).
///
/// Reads the current value (or default), applies `f`, and writes back.
/// Silently no-ops if not in an async task.
pub fn update_context<T>(key: &'static str, f: impl FnOnce(T) -> T)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let old = get_context::<T>(key).unwrap_or_default();
    let new = f(old);
    set_context(key, new);
}

// ── Value access (type-erased, for extension crates) ───────────

/// Set a raw type-erased value in the task-local store.
///
/// Used by extension crates (like dcontext-tracing) for field extraction.
/// Silently no-ops if not in an async task.
pub fn set_raw_value(key: &'static str, value: Arc<dyn ContextValue>) {
    try_apply(|store| {
        store.set_value(key, value);
    });
}

/// Set the remote scope chain on the task-local store.
///
/// Used by `deserialize_context` when restoring a cross-process context.
pub(crate) fn set_remote_chain(chain: Vec<String>) {
    try_apply(|store| store.set_remote_chain(chain));
}

/// Get a raw type-erased value from the task-local store.
///
/// Returns `None` if the key is not set or not in an async task.
pub fn get_raw_value(key: &str) -> Option<Arc<dyn ContextValue>> {
    try_apply(|store| store.get_value(key)).flatten()
}

/// Access the current context value for a key as `&dyn Any` via callback.
///
/// Returns `None` if the key has no value or not in an async task.
pub fn with_context_value<R>(key: &str, f: impl FnOnce(&dyn std::any::Any) -> R) -> Option<R> {
    get_raw_value(key).map(|arc_val| f(arc_val.as_any()))
}

// ── Snapshot / Propagation ─────────────────────────────────────

/// Take a snapshot of the current task-local context.
///
/// Used for bridging to sync context or propagating to child tasks.
/// Returns an empty snapshot if not in an async task.
pub fn snapshot() -> ContextSnapshot {
    try_apply(|store| {
        let values = store.collect_values();
        let scope_chain = store.scope_chain();
        ContextSnapshot {
            values: Arc::new(values),
            scope_chain,
        }
    })
    .unwrap_or_default()
}

/// Attach a snapshot to the task-local context by pushing a new scope
/// with its values. Returns a [`ScopeGuard`] that pops the scope on drop.
/// Returns a no-op guard if not in an async task.
pub fn attach(snap: ContextSnapshot) -> ScopeGuard {
    let guard = push_scope("");
    if !snap.scope_chain.is_empty() {
        set_remote_chain(snap.scope_chain);
    }
    for (key, val) in snap.values.iter() {
        set_raw_value(key, Arc::clone(val));
    }
    guard
}

/// Serialize the current task-local context into bytes.
pub fn serialize_context() -> Result<Vec<u8>, crate::error::ContextError> {
    let values = snapshot()
        .values
        .iter()
        .map(|(k, v)| (*k, Arc::clone(v)))
        .collect();
    let chain = scope_chain();
    crate::wire::serialize_from(values, chain)
}

/// Restore context from bytes into the task-local store.
/// Pushes a new scope with deserialized values and activates a scope
/// barrier that hides parent scopes.
pub fn deserialize_context(bytes: &[u8]) -> Result<ScopeGuard, crate::error::ContextError> {
    crate::wire::deserialize_into(bytes, true)
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
    let depth = try_apply(|store| store.push_scope(Some(name_owned)));

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

// ── Context availability ───────────────────────────────────────

/// Check whether a task-local context store is currently active.
///
/// Returns `true` if this code is running inside an async task that has
/// a task-local context store (e.g., wrapped with [`with_context`]).
/// Returns `false` if called from a blocking thread, a sync context,
/// or outside any task-local scope.
///
/// This is a lightweight check that does not take or mutate the store.
/// Use it to guard operations that should only run when async context
/// is available.
pub fn has_context() -> bool {
    TASK_CONTEXT
        .try_with(|cell| {
            // Peek: take the store, check if Some, put it back.
            let store = cell.take();
            let available = store.is_some();
            cell.set(store);
            available
        })
        .unwrap_or(false)
}

// ── Internal helpers ───────────────────────────────────────────

/// Create a forked child context from the current task-local state.
///
/// Returns a new `ContextStore` whose `frozen_parent` points to the
/// current scope. Value lookups in the child fall through to the frozen
/// parent; writes are isolated (copy-on-write).
///
/// Returns `None` if not in an async task or if the store is busy.
pub(crate) fn fork() -> Option<ContextStore> {
    try_apply(|store| store.fork_child())
}

/// Execute `f` with exclusive access to the task-local context store.
/// Returns `None` if not in an async task or if the store is busy.
fn try_apply<R>(f: impl FnOnce(&mut ContextStore) -> R) -> Option<R> {
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
