//! Sync context module — operates exclusively on thread-local storage.
//!
//! All functions in this module access the **thread-local** context store.
//! They always succeed (thread-local is always available).
//!
//! Use this module for synchronous code, `spawn_blocking` contexts, or
//! any code that needs thread-scoped context independent of async tasks.
//!
//! This module also contains the legacy public API functions (`enter_scope`,
//! `enter_named_scope`, `scope_chain`, etc.) and deprecated compatibility
//! shims (`force_thread_local`, `scope_async`, `named_scope_async`).

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::scope::ScopeGuard;
use crate::snapshot::ContextSnapshot;
use crate::store::ContextStore;
use crate::value::ContextValue;

// ── Thread-local storage ───────────────────────────────────────
//
// The store lives in `Cell<Option<ContextStore>>`.
// `Some(store)` = normal state.
// `None`        = "busy" — store was taken for modification.
//
// Thread-local is eagerly initialized to `Some(ContextStore::new())`,
// so `None` always means "busy" (never "uninitialized").

thread_local! {
    pub(crate) static CONTEXT: Cell<Option<ContextStore>> =
        Cell::new(Some(ContextStore::new()));
}

// ── Public API (explicit sync context) ─────────────────────────

/// Push a named scope onto the thread-local store.
///
/// Returns a [`ScopeGuard`] that pops the scope on drop.
/// Always succeeds (thread-local is always available).
pub fn push_scope(name: &str) -> ScopeGuard {
    let name = name.to_string();
    try_apply(|store| ScopeGuard::new(store.push_scope(Some(name))))
        .unwrap_or_else(ScopeGuard::noop)
}

/// Pop the top scope from the thread-local store.
///
/// This is typically done automatically by dropping the [`ScopeGuard`].
pub fn pop_scope(expected_depth: usize) {
    if expected_depth == usize::MAX {
        return;
    }
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

/// Get the current scope chain from the thread-local store.
pub fn scope_chain() -> Vec<String> {
    try_apply(|store| store.scope_chain()).unwrap_or_default()
}

// ── Value access (typed) ───────────────────────────────────────

/// Set a context value in the thread-local store.
pub fn set_context<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_apply(|store| {
        store.set_value(key, Arc::new(value));
    });
}

/// Get a context value from the thread-local store.
///
/// Returns `None` if the key is not set.
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
pub fn update_context<T>(key: &'static str, f: impl FnOnce(T) -> T)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let old = get_context::<T>(key).unwrap_or_default();
    let new = f(old);
    set_context(key, new);
}

// ── Value access (type-erased, for extension crates) ───────────

/// Set a raw type-erased value in the thread-local store.
///
/// Used by extension crates (like dcontext-tracing) for field extraction.
pub fn set_raw_value(key: &'static str, value: Arc<dyn ContextValue>) {
    try_apply(|store| {
        store.set_value(key, value);
    });
}

/// Get a raw type-erased value from the thread-local store.
///
/// Returns `None` if the key is not set.
pub fn get_raw_value(key: &str) -> Option<Arc<dyn ContextValue>> {
    try_apply(|store| store.get_value(key)).flatten()
}

/// Access the current context value for a key as `&dyn Any` via callback.
///
/// Returns `None` if the key has no value or the store is busy.
pub fn with_context_value<R>(key: &str, f: impl FnOnce(&dyn std::any::Any) -> R) -> Option<R> {
    get_raw_value(key).map(|arc_val| f(arc_val.as_any()))
}

// ── Snapshot / Restore ─────────────────────────────────────────

/// Take a snapshot of the current thread-local context.
///
/// Used for propagating context to spawned threads or bridging to async.
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

/// Initialize/reset the thread-local context from a snapshot.
///
/// Used when bridging from async context to a sync thread
/// (e.g., in `spawn_blocking`).
///
/// # Example
///
/// ```rust,ignore
/// let snap = dcontext::async_ctx::snapshot();
/// tokio::task::spawn_blocking(move || {
///     dcontext::sync_ctx::restore(snap);
///     do_blocking_work();
/// }).await;
/// ```
pub fn restore(snapshot: ContextSnapshot) {
    try_apply(|store| {
        let chain = snapshot.scope_chain.clone();
        let values: HashMap<&'static str, Arc<dyn ContextValue>> = snapshot
            .values
            .iter()
            .map(|(k, v)| (*k, Arc::clone(v)))
            .collect();
        *store = ContextStore::from_values_with_chain(values, chain);
    });
}

/// Clear the thread-local context entirely.
pub fn clear() {
    try_apply(|store| {
        *store = ContextStore::new();
    });
}

/// Attach a snapshot to the thread-local context by pushing a new scope
/// with its values. Returns a [`ScopeGuard`] that pops the scope on drop.
pub fn attach(snap: ContextSnapshot) -> ScopeGuard {
    let guard = enter_scope();
    if !snap.scope_chain.is_empty() {
        set_remote_chain(snap.scope_chain);
    }
    for (key, val) in snap.values.iter() {
        set_value(key, Arc::clone(val));
    }
    guard
}

/// Serialize the current thread-local context into bytes.
pub fn serialize_context() -> Result<Vec<u8>, crate::error::ContextError> {
    crate::wire::serialize_from(collect_values(), collect_scope_chain())
}

/// Restore context from bytes into the thread-local store.
/// Pushes a new scope with deserialized values and activates a scope
/// barrier that hides parent scopes.
pub fn deserialize_context(bytes: &[u8]) -> Result<ScopeGuard, crate::error::ContextError> {
    crate::wire::deserialize_into(bytes, false)
}

// ── Legacy scope management (re-exported at crate root) ────────

/// Push a new scope and return a guard.
/// Returns a no-op guard if the store is busy (re-entrant access).
pub fn enter_scope() -> ScopeGuard {
    try_apply(|store| ScopeGuard::new(store.push_scope(None))).unwrap_or_else(ScopeGuard::noop)
}

/// Push a new **named** scope and return a guard.
/// Returns a no-op guard if the store is busy (re-entrant access).
///
/// Named scopes appear in [`scope_chain()`] — they form a lightweight call
/// stack that is propagated across process boundaries.
pub fn enter_named_scope(name: impl Into<String>) -> ScopeGuard {
    let name = name.into();
    try_apply(|store| ScopeGuard::new(store.push_scope(Some(name))))
        .unwrap_or_else(ScopeGuard::noop)
}

/// Pop a scope (called by ScopeGuard::drop).
/// Silently skips if the store is busy (re-entrant access) or if the
/// guard is a noop sentinel (expected_depth == usize::MAX).
pub(crate) fn leave_scope(expected_depth: usize) {
    if expected_depth == usize::MAX {
        return; // noop guard
    }
    // Garbage (old Arc values) is returned from try_apply and dropped here,
    // outside the Cell window, so any user Drop code runs with a valid store.
    let _garbage = try_apply(|store| store.pop_scope(expected_depth));
}

// ── Fork support ───────────────────────────────────────────────

/// Create a forked child context from the current thread-local state.
///
/// Returns a new `ContextStore` whose `frozen_parent` points to the
/// current scope. Value lookups in the child fall through to the frozen
/// parent; writes are isolated (copy-on-write).
///
/// Returns `None` if the store is busy (re-entrant access).
pub(crate) fn fork() -> Option<crate::store::ContextStore> {
    try_apply(|store| store.fork_child())
}

// ── Value access (internal, used by lib.rs dispatch) ───────────


/// Set a value in the current scope.
/// Silently skips if the store is busy (re-entrant access).
/// Old value is dropped outside the Cell window.
pub(crate) fn set_value(key: &'static str, value: Arc<dyn ContextValue>) {
    let _old = try_apply(|store| store.set_value(key, value));
}

/// Collect all effective values (for snapshot/serialization).
/// Returns an empty map if the store is busy.
pub(crate) fn collect_values() -> HashMap<&'static str, Arc<dyn ContextValue>> {
    try_apply(|store| store.collect_values()).unwrap_or_default()
}

/// Collect the scope chain for serialization.
pub(crate) fn collect_scope_chain() -> Vec<String> {
    scope_chain()
}

/// Store a remote scope chain in the current context.
/// Silently skips if the store is busy (re-entrant access).
pub(crate) fn set_remote_chain(chain: Vec<String>) {
    try_apply(|store| store.set_remote_chain(chain));
}

// ── Internal helpers ───────────────────────────────────────────

/// Execute `f` with exclusive access to the thread-local context store.
/// Returns `None` if the store is busy (re-entrant access) or if the
/// thread-local is being destroyed during thread shutdown.
///
/// The Cell is restored (`set(Some(store))`) before the return value is
/// dropped, so any user Drop code runs with a valid store in the Cell.
pub(crate) fn try_apply<R>(f: impl FnOnce(&mut ContextStore) -> R) -> Option<R> {
    CONTEXT
        .try_with(|cell| {
            let mut store = cell.take()?; // None = busy
            let result = f(&mut store);
            cell.set(Some(store));
            Some(result)
        })
        .unwrap_or(None) // Err = thread-local is being destroyed
}
