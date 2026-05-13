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
//!
//! ## Quick Start
//!
//! ```rust
//! use dcontext::{RegistryBuilder, initialize};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Clone, Default, Debug, Serialize, Deserialize)]
//! struct RequestId(String);
//!
//! # fn main() {
//! let mut builder = RegistryBuilder::new();
//! builder.register::<RequestId>("request_id");
//! initialize(builder);
//!
//! let _scope = dcontext::push_scope("ingress");
//! dcontext::set_context("request_id", RequestId("req-123".into()));
//!
//! let rid: Option<RequestId> = dcontext::get_context("request_id");
//! assert_eq!(rid.unwrap().0, "req-123");
//!
//! let chain = dcontext::scope_chain();
//! assert_eq!(chain, vec!["ingress"]);
//! # }
//! ```

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

// Keep sync_ctx as internal (thread-local + try_apply live here)
pub(crate) mod sync_ctx;

// Re-export public types
pub use attach::AttachGuard;
pub use error::ContextError;
pub use future_ext::{ContextFutureExt, WithContext};
pub use scope::ScopeGuard;
pub use snapshot::ContextSnapshot;

#[cfg(feature = "context-key")]
pub use context_key::ContextKey;

// Registration
pub use registry::{
    initialize, keys_with_metadata, try_initialize, with_metadata, RegistrationOptions,
    RegistryBuilder,
};

#[cfg(test)]
pub(crate) use registry::{
    register, register_local, register_migration, register_with, try_register, try_register_local,
    try_register_migration, try_register_with,
};

// Serialization helpers
pub use wire::{make_wire_bytes, make_wire_bytes_v};

// Configuration
pub use config::{
    max_context_size, max_scope_chain_len, set_max_context_size, set_max_scope_chain_len,
};

use std::collections::HashMap;
use std::sync::Arc;

use crate::store::ContextStore;
use crate::value::ContextValue;

/// Push a named scope onto the context store.
///
/// Returns a [`ScopeGuard`] that pops the scope on drop.
pub fn push_scope(name: &str) -> ScopeGuard {
    sync_ctx::push_scope(name)
}

/// Get the current scope chain.
pub fn scope_chain() -> Vec<String> {
    sync_ctx::scope_chain()
}

/// Set a context value.
pub fn set_context<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    sync_ctx::set_context(key, value)
}

/// Get a context value. Returns `None` if the key is not set.
pub fn get_context<T>(key: &str) -> Option<T>
where
    T: Clone + Send + Sync + 'static,
{
    sync_ctx::get_context(key)
}

/// Update a context value using a callback (read-modify-write).
pub fn update_context<T>(key: &'static str, f: impl FnOnce(T) -> T)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    sync_ctx::update_context(key, f)
}

/// Set a raw type-erased value.
pub fn set_raw_value(key: &'static str, value: Arc<dyn ContextValue>) {
    sync_ctx::set_raw_value(key, value)
}

/// Get a raw type-erased value.
pub fn get_raw_value(key: &str) -> Option<Arc<dyn ContextValue>> {
    sync_ctx::get_raw_value(key)
}

/// Take a snapshot of the current context (for serialization or cross-process transfer).
pub fn snapshot() -> ContextSnapshot {
    sync_ctx::snapshot()
}

/// Fork the current context. Creates a child store with a frozen parent.
///
/// Value lookups fall through to the frozen parent (cheap, Arc-shared).
/// Writes are isolated in the child (copy-on-write).
pub fn fork() -> ContextStore {
    sync_ctx::fork().unwrap_or_else(ContextStore::new)
}

/// Push a new scope with snapshot values merged in.
///
/// The snapshot's scope-chain is ignored - the current chain is preserved
/// and the new scope name is appended. Only the snapshot's values are
/// merged into the new scope.
pub fn push_scope_with_snapshot(name: &str, snap: ContextSnapshot) -> ScopeGuard {
    let guard = sync_ctx::push_scope(name);
    for (key, val) in snap.values.iter() {
        sync_ctx::set_value(key, Arc::clone(val));
    }
    guard
}

/// Attach a snapshot as root context. Replaces entire thread-local store.
///
/// The scope hierarchy is flattened (values only), but the snapshot's scope
/// names are preserved for display in `scope_chain()`.
/// Returns an [`AttachGuard`] that restores the previous store on drop.
pub fn attach_snapshot(snap: ContextSnapshot) -> AttachGuard {
    let values: HashMap<&'static str, Arc<dyn ContextValue>> = snap
        .values
        .iter()
        .map(|(k, v)| (*k, Arc::clone(v)))
        .collect();
    let new_store = ContextStore::from_values_with_chain(values, snap.scope_chain);
    attach_store(new_store)
}

/// Attach a `ContextStore` as root context. Replaces entire thread-local store.
///
/// Returns an [`AttachGuard`] that restores the previous store on drop.
pub fn attach_store(store: ContextStore) -> AttachGuard {
    let prev = sync_ctx::CONTEXT.with(|cell| cell.replace(Some(store)));
    AttachGuard::new(prev)
}

/// Serialize the current context into bytes (wire format).
pub fn serialize_context() -> Result<Vec<u8>, ContextError> {
    sync_ctx::serialize_context()
}

/// Deserialize bytes into the current context. Pushes a new scope with
/// a scope barrier hiding parent scopes.
pub fn deserialize_context(bytes: &[u8]) -> Result<ScopeGuard, ContextError> {
    sync_ctx::deserialize_context(bytes)
}

/// Deserialize bytes into a standalone `ContextSnapshot` without modifying
/// the current store.
pub fn deserialize_to_snapshot(bytes: &[u8]) -> Result<ContextSnapshot, ContextError> {
    wire::deserialize_to_snapshot(bytes)
}

/// Convert a `ContextSnapshot` into a `ContextStore` (for use with `with_context`).
pub fn snapshot_to_store(snap: ContextSnapshot) -> ContextStore {
    let values: HashMap<&'static str, Arc<dyn ContextValue>> = snap
        .values
        .iter()
        .map(|(k, v)| (*k, Arc::clone(v)))
        .collect();
    ContextStore::from_values_with_chain(values, snap.scope_chain)
}

/// Clear the context entirely.
pub fn clear() {
    sync_ctx::clear()
}

/// Peek at the current scope depth.
pub fn current_depth() -> Option<usize> {
    sync_ctx::current_depth()
}

#[cfg(test)]
mod tests;
