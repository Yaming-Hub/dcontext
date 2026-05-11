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
        // Reset to a fresh store built from the snapshot
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
