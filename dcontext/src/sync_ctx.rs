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
    with_context(|store| ScopeGuard::new(store.push_scope(Some(name))))
        .unwrap_or_else(ScopeGuard::noop)
}

/// Pop the top scope from the thread-local store.
///
/// This is typically done automatically by dropping the [`ScopeGuard`].
pub fn pop_scope(expected_depth: usize) {
    if expected_depth == usize::MAX {
        return;
    }
    let _garbage = with_context(|store| store.pop_scope(expected_depth));
}

/// Get the current scope chain from the thread-local store.
pub fn scope_chain() -> Vec<String> {
    with_context(|store| store.scope_chain()).unwrap_or_default()
}

// ── Value access (typed) ───────────────────────────────────────

/// Set a context value in the thread-local store.
pub fn set_context<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    with_context(|store| {
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
    with_context(|store| {
        store.get_value(key).and_then(|arc| {
            arc.as_any().downcast_ref::<T>().cloned()
        })
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
    with_context(|store| {
        store.set_value(key, value);
    });
}

/// Get a raw type-erased value from the thread-local store.
///
/// Returns `None` if the key is not set.
pub fn get_raw_value(key: &str) -> Option<Arc<dyn ContextValue>> {
    with_context(|store| store.get_value(key)).flatten()
}

/// Access the current context value for a key as `&dyn Any` via callback.
///
/// Returns `None` if the key has no value or the store is busy.
pub fn with_context_value<R>(
    key: &str,
    f: impl FnOnce(&dyn std::any::Any) -> R,
) -> Option<R> {
    get_raw_value(key).map(|arc_val| f(arc_val.as_any()))
}

// ── Snapshot / Restore ─────────────────────────────────────────

/// Take a snapshot of the current thread-local context.
///
/// Used for propagating context to spawned threads or bridging to async.
pub fn snapshot() -> ContextSnapshot {
    with_context(|store| {
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
    with_context(|store| {
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
    with_context(|store| {
        *store = ContextStore::new();
    });
}

// ── Legacy scope management (re-exported at crate root) ────────

/// Push a new scope and return a guard.
/// Returns a no-op guard if the store is busy (re-entrant access).
pub fn enter_scope() -> ScopeGuard {
    with_context(|store| ScopeGuard::new(store.push_scope(None)))
        .unwrap_or_else(ScopeGuard::noop)
}

/// Push a new **named** scope and return a guard.
/// Returns a no-op guard if the store is busy (re-entrant access).
///
/// Named scopes appear in [`scope_chain()`] — they form a lightweight call
/// stack that is propagated across process boundaries.
pub fn enter_named_scope(name: impl Into<String>) -> ScopeGuard {
    let name = name.into();
    with_context(|store| ScopeGuard::new(store.push_scope(Some(name))))
        .unwrap_or_else(ScopeGuard::noop)
}

/// Pop a scope (called by ScopeGuard::drop).
/// Silently skips if the store is busy (re-entrant access) or if the
/// guard is a noop sentinel (expected_depth == usize::MAX).
pub(crate) fn leave_scope(expected_depth: usize) {
    if expected_depth == usize::MAX {
        return; // noop guard
    }
    // Garbage (old Arc values) is returned from with_context and dropped here,
    // outside the Cell window, so any user Drop code runs with a valid store.
    let _garbage = with_context(|store| store.pop_scope(expected_depth));
}

// ── Deprecated compatibility ───────────────────────────────────

/// No-op compatibility shim. With the dual-context redesign, there is no
/// dispatch logic to override — all access in this module is already
/// thread-local. This function simply calls `f()` and returns the result.
#[deprecated(note = "No longer needed: sync_ctx always uses thread-local. Will be removed in a future release.")]
pub fn force_thread_local<R>(f: impl FnOnce() -> R) -> R {
    f()
}

/// Deprecated: use `async_ctx::scope()` instead.
/// For backward compatibility, dispatches to task-local if available.
#[deprecated(note = "Use async_ctx::scope() for task-local scoping.")]
pub async fn scope_async<F, R>(f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    // If task-local is available, use it (old dispatch behavior)
    if crate::async_ctx::current_depth().is_some() {
        return crate::async_ctx::scope("", f).await;
    }
    let depth = with_context(|store| store.push_scope(None));

    match depth {
        None => f.await,
        Some(depth) => {
            let cleanup = ScopeCleanup(depth);
            let result = f.await;
            std::mem::forget(cleanup);
            leave_scope(depth);
            result
        }
    }
}

/// Deprecated: use `async_ctx::scope()` instead.
/// For backward compatibility, dispatches to task-local if available.
#[deprecated(note = "Use async_ctx::scope() for task-local scoping.")]
pub async fn named_scope_async<F, R>(name: impl Into<String>, f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    let name = name.into();
    // If task-local is available, use it (old dispatch behavior)
    if crate::async_ctx::current_depth().is_some() {
        return crate::async_ctx::scope(&name, f).await;
    }
    let depth = with_context(|store| store.push_scope(Some(name)));

    match depth {
        None => f.await,
        Some(depth) => {
            let cleanup = ScopeCleanup(depth);
            let result = f.await;
            std::mem::forget(cleanup);
            leave_scope(depth);
            result
        }
    }
}

/// RAII guard that pops a scope during unwind if the async future panics.
/// On the normal path the caller calls `std::mem::forget(cleanup)` to disarm it.
struct ScopeCleanup(usize);

impl Drop for ScopeCleanup {
    fn drop(&mut self) {
        leave_scope(self.0);
    }
}

// ── Fork support ───────────────────────────────────────────────

/// Create a ForkHandle from the current context state.
/// Returns None if the store is busy.
pub(crate) fn do_fork() -> Option<crate::fork::ForkHandle> {
    with_context(|store| crate::fork::create_fork_handle(store))
}

// ── Value access (internal, used by lib.rs dispatch) ───────────

/// Get a value from the context. Returns an Arc clone.
/// Returns `None` if the key is not set or the store is busy.
pub(crate) fn get_value(key: &str) -> Option<Arc<dyn ContextValue>> {
    with_context(|store| store.get_value(key)).flatten()
}

/// Set a value in the current scope.
/// Silently skips if the store is busy (re-entrant access).
/// Old value is dropped outside the Cell window.
pub(crate) fn set_value(key: &'static str, value: Arc<dyn ContextValue>) {
    let _old = with_context(|store| store.set_value(key, value));
}

/// Collect all effective values (for snapshot/serialization).
/// Returns an empty map if the store is busy.
pub(crate) fn collect_values() -> HashMap<&'static str, Arc<dyn ContextValue>> {
    with_context(|store| store.collect_values())
        .unwrap_or_default()
}

/// Collect the scope chain for serialization.
pub(crate) fn collect_scope_chain() -> Vec<String> {
    scope_chain()
}

/// Store a remote scope chain in the current context.
/// Silently skips if the store is busy (re-entrant access).
pub(crate) fn set_remote_chain(chain: Vec<String>) {
    with_context(|store| store.set_remote_chain(chain));
}

// ── Internal helpers ───────────────────────────────────────────

/// Execute `f` with exclusive access to the thread-local context store.
/// Returns `None` if the store is busy (re-entrant access) or if the
/// thread-local is being destroyed during thread shutdown.
///
/// The Cell is restored (`set(Some(store))`) before the return value is
/// dropped, so any user Drop code runs with a valid store in the Cell.
fn with_context<R>(f: impl FnOnce(&mut ContextStore) -> R) -> Option<R> {
    let f = std::cell::Cell::new(Some(f));
    let run = |cell: &Cell<Option<ContextStore>>| -> Option<R> {
        let mut store = cell.take()?; // None = busy
        let func = f.take().expect("with_context closure called more than once");
        let result = func(&mut store);
        cell.set(Some(store));
        Some(result)
    };
    match CONTEXT.try_with(run) {
        Ok(r) => r,
        Err(_) => {
            // Thread-local is being destroyed — provide an empty Cell
            // so callers see None ("busy") and return defaults.
            let temp = Cell::new(None);
            run(&temp)
        }
    }
}
