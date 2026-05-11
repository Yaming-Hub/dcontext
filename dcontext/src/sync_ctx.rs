//! Sync context module — operates exclusively on thread-local storage.
//!
//! All functions in this module access the **thread-local** context store.
//! They always succeed (thread-local is always available).
//!
//! Use this module for synchronous code, `spawn_blocking` contexts, or
//! any code that needs thread-scoped context independent of async tasks.

use std::collections::HashMap;
use std::sync::Arc;

use crate::scope::ScopeGuard;
use crate::snapshot::ContextSnapshot;
use crate::sync_storage::{ContextStore, CONTEXT};
use crate::value::ContextValue;

// ── Scope management ───────────────────────────────────────────

/// Push a named scope onto the thread-local store.
///
/// Returns a [`ScopeGuard`] that pops the scope on drop.
/// Always succeeds (thread-local is always available).
pub fn push_scope(name: &str) -> ScopeGuard {
    let name = name.to_string();
    with_thread_store(|store| ScopeGuard::new(store.push_scope(Some(name))))
        .unwrap_or_else(ScopeGuard::noop)
}

/// Pop the top scope from the thread-local store.
///
/// This is typically done automatically by dropping the [`ScopeGuard`].
pub fn pop_scope(expected_depth: usize) {
    if expected_depth == usize::MAX {
        return;
    }
    let _garbage = with_thread_store(|store| store.pop_scope(expected_depth));
}

/// Get the current scope chain from the thread-local store.
pub fn scope_chain() -> Vec<String> {
    with_thread_store(|store| store.scope_chain()).unwrap_or_default()
}

// ── Value access (typed) ───────────────────────────────────────

/// Set a context value in the thread-local store.
pub fn set_context<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    with_thread_store(|store| {
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
    with_thread_store(|store| {
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
    with_thread_store(|store| {
        store.set_value(key, value);
    });
}

/// Get a raw type-erased value from the thread-local store.
///
/// Returns `None` if the key is not set.
pub fn get_raw_value(key: &str) -> Option<Arc<dyn ContextValue>> {
    with_thread_store(|store| store.get_value(key)).flatten()
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
    with_thread_store(|store| {
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
    with_thread_store(|store| {
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
    with_thread_store(|store| {
        *store = ContextStore::new();
    });
}

// ── Internal helpers ───────────────────────────────────────────

/// Execute `f` with exclusive access to the thread-local context store.
/// Returns `None` if the store is busy (re-entrant access).
fn with_thread_store<R>(f: impl FnOnce(&mut ContextStore) -> R) -> Option<R> {
    match CONTEXT.try_with(|cell| {
        let mut store = cell.take()?;
        let r = f(&mut store);
        cell.set(Some(store));
        Some(r)
    }) {
        Ok(r) => r,
        Err(_) => None,
    }
}
