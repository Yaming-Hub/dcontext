//! # dcontext
//!
//! Distributed context propagation for Rust.
//!
//! `dcontext` provides a scoped, type-safe key-value store that travels with
//! the execution flow - across function calls, async/sync boundaries, thread
//! spawns, and even process boundaries via serialization.
//!
//! ## Architecture
//!
//! A single `thread_local!` store is the source of truth. For async code,
//! the [`WithContext`] future wrapper swaps the store in/out on each poll,
//! making it effectively task-local without any runtime dependency.

pub mod error;
mod registry;
mod scope;
mod snapshot;
pub(crate) mod store;
pub mod value;
mod wire;

mod attach;
mod config;
#[cfg(feature = "context-key")]
mod context_key;
mod future_ext;
#[macro_use]
mod macros;

// Re-export public types
pub use attach::AttachGuard;
pub use error::ContextError;
pub use future_ext::{ContextFutureExt, WithContext};
pub use scope::ScopeGuard;
pub use snapshot::ContextSnapshot;
pub use store::ContextStore;

#[cfg(feature = "context-key")]
pub use context_key::ContextKey;

// Registration
pub use registry::{
    initialize, keys_with_metadata, try_initialize, with_metadata, RegistrationOptions,
    RegistryBuilder,
};

#[cfg(test)]
pub(crate) use registry::{
    register, register_migration, register_with, try_register, try_register_migration,
    try_register_with,
};

// Serialization helpers
pub use wire::{make_wire_bytes, make_wire_bytes_v};

// Configuration
pub use config::{
    max_context_size, max_scope_chain_len, set_max_context_size, set_max_scope_chain_len,
};

use std::collections::HashMap;
use std::sync::Arc;

use crate::store::{try_apply, CONTEXT};
use crate::value::ContextValue;

// ── Public API ─────────────────────────────────────────────────

pub(crate) fn capture_with_registry(
    store: &ContextStore,
    registry: &registry::Registry<'_>,
) -> ContextSnapshot {
    let values: HashMap<&'static str, Arc<dyn ContextValue>> = store
        .collect_values()
        .into_iter()
        .filter(|(k, _)| !registry.is_local_key(k))
        .collect();
    let scope_chain = store.scope_chain();
    ContextSnapshot {
        values: Arc::new(values),
        scope_chain,
    }
}

pub(crate) fn store_from_snapshot_with_registry(
    snap: ContextSnapshot,
    registry: &registry::Registry<'_>,
) -> ContextStore {
    let ContextSnapshot {
        values,
        scope_chain,
    } = snap;
    let values: HashMap<&'static str, Arc<dyn ContextValue>> = values
        .iter()
        .filter(|(k, v)| registry.is_valid_value(k, v.as_ref()) && !registry.is_local_key(k))
        .map(|(k, v)| (*k, Arc::clone(v)))
        .collect();
    ContextStore::from_values_with_chain(values, scope_chain)
}

/// Push a named scope onto the context store.
/// Returns a [`ScopeGuard`] that pops the scope on drop.
///
/// Scopes form a hierarchical chain queryable via [`scope_chain()`].
/// Use `.scope("name")` on futures for async code (see [`ContextFutureExt::scope`]).
///
/// # Note
/// The returned `ScopeGuard` is `!Send` — it must be dropped on the same thread.
/// For async code in multi-threaded runtimes, prefer `future.scope("name")` instead.
pub fn push_scope(name: &str) -> ScopeGuard {
    let name = name.to_string();
    registry::with_global_registry(|registry| {
        try_apply(|store| ScopeGuard::new(store.push_scope(registry, Some(name))))
            .unwrap_or_else(ScopeGuard::noop)
    })
}

/// Get the current scope chain — the ordered list of named scopes from root to current.
///
/// This is the primary observability integration point: use `scope_chain()` to record
/// the current execution path into tracing spans, logs, or metrics.
///
/// Returns an empty vec if no scopes are active.
pub fn scope_chain() -> Vec<String> {
    try_apply(|store| store.scope_chain()).unwrap_or_default()
}

/// Set a context variable.
pub fn set_context_variable<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_apply(|store| {
        store.set_value(key, Arc::new(value));
    });
}

/// Get a context variable. Returns `None` if the key is not set.
pub fn get_context_variable<T>(key: &str) -> Option<T>
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

/// Update a context variable using a callback (read-modify-write).
pub fn update_context_variable<T>(key: &'static str, f: impl FnOnce(T) -> T)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    let old = get_context_variable::<T>(key).unwrap_or_default();
    let new = f(old);
    set_context_variable(key, new);
}

/// Capture a snapshot of the current context.
/// Local-only variables (registered with `.local_only()`) are excluded.
pub fn capture() -> ContextSnapshot {
    registry::with_global_registry(|registry| {
        try_apply(|store| capture_with_registry(store, registry)).unwrap_or_default()
    })
}

/// Fork the current context. Creates a child store with a frozen parent.
/// Value lookups fall through to the frozen parent (cheap, Arc-shared).
/// Writes are isolated in the child (copy-on-write).
pub fn fork() -> ContextStore {
    try_apply(|store| store.fork_child()).unwrap_or_else(ContextStore::new)
}

/// Capture a snapshot of the current context and serialize it to wire-format bytes.
///
/// Convenience method equivalent to `capture().serialize()`.
/// Local-only variables are excluded.
pub fn capture_serialized() -> Result<Vec<u8>, error::ContextError> {
    capture().serialize()
}

/// Deserialize wire-format bytes and attach as root context.
///
/// Convenience method equivalent to:
/// ```ignore
/// let snap = ContextSnapshot::deserialize(bytes)?;
/// Ok(attach_snapshot(snap))
/// ```
///
/// Returns an [`AttachGuard`] that restores the previous context on drop.
pub fn attach_from_bytes(bytes: &[u8]) -> Result<AttachGuard, error::ContextError> {
    let snap = ContextSnapshot::deserialize(bytes)?;
    Ok(attach_snapshot(snap))
}

/// Attach a snapshot as root context. Returns an [`AttachGuard`] that restores previous state.
///
/// # Note
/// The returned `AttachGuard` is `!Send` — it must be dropped on the same thread.
/// For async code, prefer `future.attach(snap)` instead.
pub fn attach_snapshot(snap: ContextSnapshot) -> AttachGuard {
    let store: ContextStore = snap.into();
    attach_store(store)
}

/// Attach a snapshot and push a named scope in one call.
///
/// Equivalent to:
/// ```ignore
/// let _attach = attach_snapshot(snap);
/// let _scope = push_scope(name);
/// ```
///
/// Returns a combined guard that pops the scope and restores the previous context on drop.
/// Useful at inbound boundaries where every attach point should have a scope name.
pub fn attach_snapshot_with_scope(snap: ContextSnapshot, name: &str) -> (AttachGuard, ScopeGuard) {
    let guard = attach_snapshot(snap);
    let scope = push_scope(name);
    (guard, scope)
}

/// Attach a `ContextStore` as root context. Returns an [`AttachGuard`].
pub fn attach_store(store: ContextStore) -> AttachGuard {
    let prev = std::thread::LocalKey::with(&CONTEXT, |cell| cell.replace(Some(store)));
    AttachGuard::new(prev)
}

/// Merge values from another store into the current context.
/// Only merges values, not scope chain.
pub fn merge_with(source: ContextStore) {
    let values = source.collect_values();
    try_apply(|store| {
        for (key, val) in values {
            store.set_value(key, val);
        }
    });
}

/// Clear the context entirely.
pub fn clear() {
    try_apply(|store| {
        *store = ContextStore::new();
    });
}

// ── From<ContextSnapshot> for ContextStore ─────────────────────

impl From<ContextSnapshot> for ContextStore {
    fn from(snap: ContextSnapshot) -> Self {
        registry::with_global_registry(|registry| store_from_snapshot_with_registry(snap, registry))
    }
}

#[cfg(test)]
mod tests;
