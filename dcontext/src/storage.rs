use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::scope::{ContextStore, ScopeGuard};
use crate::value::ContextValue;

// ── Thread-local / task-local storage ──────────────────────────
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
    static FORCE_THREAD_LOCAL_DEPTH: Cell<u32> = const { Cell::new(0) };
}

#[cfg(feature = "tokio")]
tokio::task_local! {
    pub(crate) static TASK_CONTEXT: Cell<Option<ContextStore>>;
}

// ── Cell dispatch ──────────────────────────────────────────────

/// Dispatch to the appropriate Cell (task-local or thread-local).
fn with_current_cell<R>(mut f: impl FnMut(&Cell<Option<ContextStore>>) -> R) -> R {
    #[cfg(feature = "tokio")]
    {
        let force = FORCE_THREAD_LOCAL_DEPTH.with(|c| c.get()) > 0;
        if !force {
            let result: Cell<Option<R>> = Cell::new(None);
            let found = TASK_CONTEXT.try_with(|cell| {
                result.set(Some(f(cell)));
            });
            if found.is_ok() {
                return result.into_inner()
                    .expect("invariant: closure set the result when try_with succeeded");
            }
            // No task-local — fall through to thread-local.
        }
    }

    CONTEXT.with(|cell| f(cell))
}

/// Execute `f` with exclusive access to the context store.
/// Returns `None` if the store is busy (re-entrant access).
///
/// The Cell is restored (`set(Some(store))`) before the return value is
/// dropped, so any user Drop code runs with a valid store in the Cell.
fn with_store<R>(f: impl FnOnce(&mut ContextStore) -> R) -> Option<R> {
    let f = std::cell::Cell::new(Some(f));
    with_current_cell(|cell| {
        let mut store = cell.take()?; // None = busy
        let func = f.take().expect("with_store closure called more than once");
        let result = func(&mut store);
        cell.set(Some(store));
        Some(result)
    })
}

// ── Scope management ───────────────────────────────────────────

/// Push a new scope and return a guard.
/// Returns a no-op guard if the store is busy (re-entrant access).
pub fn enter_scope() -> ScopeGuard {
    with_store(|store| ScopeGuard::new(store.push_scope(None)))
        .unwrap_or_else(ScopeGuard::noop)
}

/// Push a new **named** scope and return a guard.
/// Returns a no-op guard if the store is busy (re-entrant access).
///
/// Named scopes appear in [`scope_chain()`] — they form a lightweight call
/// stack that is propagated across process boundaries.
pub fn enter_named_scope(name: impl Into<String>) -> ScopeGuard {
    let name = name.into();
    with_store(|store| ScopeGuard::new(store.push_scope(Some(name))))
        .unwrap_or_else(ScopeGuard::noop)
}

/// Pop a scope (called by ScopeGuard::drop).
/// Silently skips if the store is busy (re-entrant access) or if the
/// guard is a noop sentinel (expected_depth == usize::MAX).
pub(crate) fn leave_scope(expected_depth: usize) {
    if expected_depth == usize::MAX {
        return; // noop guard
    }
    // Garbage (old Arc values) is returned from with_store and dropped here,
    // outside the Cell window, so any user Drop code runs with a valid store.
    let _garbage = with_store(|store| store.pop_scope(expected_depth));
}

/// Execute `f` in a new scope. Changes revert when `f` returns.
pub fn scope<R>(f: impl FnOnce() -> R) -> R {
    let _guard = enter_scope();
    f()
}

/// RAII guard that pops a scope during unwind if the async future panics.
/// On the normal path the caller calls `std::mem::forget(cleanup)` to disarm it.
#[cfg(feature = "tokio")]
struct ScopeCleanup(usize);

#[cfg(feature = "tokio")]
impl Drop for ScopeCleanup {
    fn drop(&mut self) {
        leave_scope(self.0);
    }
}

/// Async version: execute a future in a new scope.
/// The scope is entered before polling and exited after the future completes.
/// If the future panics, the scope is cleaned up during unwind.
#[cfg(feature = "tokio")]
pub async fn scope_async<F, R>(f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    let depth = with_store(|store| store.push_scope(None));

    match depth {
        None => f.await, // store busy — run without scope
        Some(depth) => {
            let cleanup = ScopeCleanup(depth);
            let result = f.await;
            std::mem::forget(cleanup);
            leave_scope(depth);
            result
        }
    }
}

/// Async version of [`enter_named_scope`]: execute a future in a new **named** scope.
///
/// Like [`scope_async`], this avoids holding the `!Send` [`ScopeGuard`] across
/// `.await` points by manually managing push/pop. If the future panics, the
/// scope is cleaned up during unwind.
///
/// Named scopes appear in [`scope_chain()`] and propagate across process
/// boundaries via serialization.
#[cfg(feature = "tokio")]
pub async fn named_scope_async<F, R>(name: impl Into<String>, f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    let name = name.into();
    let depth = with_store(|store| store.push_scope(Some(name)));

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

// ── Value access ───────────────────────────────────────────────

/// Get a value from the context. Returns an Arc clone.
/// Returns `None` if the key is not set or the store is busy.
pub(crate) fn get_value(key: &str) -> Option<Arc<dyn ContextValue>> {
    with_store(|store| store.get_value(key)).flatten()
}

/// Set a value in the current scope.
/// Silently skips if the store is busy (re-entrant access).
/// Old value is dropped outside the Cell window.
pub(crate) fn set_value(key: &'static str, value: Arc<dyn ContextValue>) {
    let _old = with_store(|store| store.set_value(key, value));
    // _old: Option<Option<Arc<dyn ContextValue>>> — dropped here, outside Cell window
}

/// Collect all effective values (for snapshot/serialization).
/// Returns an empty map if the store is busy.
pub(crate) fn collect_values() -> HashMap<&'static str, Arc<dyn ContextValue>> {
    with_store(|store| store.collect_values())
        .unwrap_or_default()
}

/// Return the current scope chain: remote prefix + local named scope names.
///
/// Returns an empty `Vec` if the store is busy (re-entrant access from
/// tracing callbacks, etc.).
pub fn scope_chain() -> Vec<String> {
    with_store(|store| store.scope_chain())
        .unwrap_or_default()
}

/// Collect the scope chain for serialization.
pub(crate) fn collect_scope_chain() -> Vec<String> {
    scope_chain()
}

/// Store a remote scope chain in the current context.
/// Silently skips if the store is busy (re-entrant access).
pub(crate) fn set_remote_chain(chain: Vec<String>) {
    with_store(|store| store.set_remote_chain(chain));
}

// ── Thread-local escape hatch ──────────────────────────────────

/// Escape hatch: explicitly use thread-local storage even inside an async runtime.
/// Panic-safe and nesting-safe via depth counter + RAII guard.
pub fn force_thread_local<R>(f: impl FnOnce() -> R) -> R {
    FORCE_THREAD_LOCAL_DEPTH.with(|c| c.set(c.get() + 1));

    struct DepthGuard;
    impl Drop for DepthGuard {
        fn drop(&mut self) {
            crate::storage::FORCE_THREAD_LOCAL_DEPTH.with(|c| c.set(c.get() - 1));
        }
    }
    let _guard = DepthGuard;

    f()
}
