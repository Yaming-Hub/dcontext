//! # dcontext
//!
//! Distributed context propagation for Rust.
//!
//! `dcontext` provides a scoped, type-safe key–value store that travels with
//! the execution flow — across function calls, async/sync boundaries, thread
//! spawns, and even process boundaries via serialization.
//!
//! ## Quick Start
//!
//! ```rust
//! use dcontext::{RegistryBuilder, initialize, enter_named_scope, get_context, set_context, scope_chain};
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Clone, Default, Debug, Serialize, Deserialize)]
//! struct RequestId(String);
//!
//! # fn main() {
//! let mut builder = RegistryBuilder::new();
//! builder.register::<RequestId>("request_id");
//! initialize(builder); // freeze registry — all reads are lock-free
//!
//! let _guard = enter_named_scope("ingress");
//! set_context("request_id", RequestId("req-123".into()));
//!
//! let rid: RequestId = get_context("request_id");
//! assert_eq!(rid.0, "req-123");
//!
//! // Query the scope chain — returns names of all active named scopes
//! let chain = scope_chain(); // vec!["ingress"]
//! # }
//! ```
//!
//! See also: [`enter_scope`] for unnamed scopes, [`named_scope_async`] for
//! async named scopes, and [`scope_chain`] for querying the full distributed
//! call path (including remote prefix from cross-process propagation).

pub mod error;
pub mod value;
mod registry;
mod scope;
pub(crate) mod async_storage;
pub(crate) mod sync_storage;
mod snapshot;
mod wire;
mod helpers;
#[cfg(feature = "context-key")]
mod context_key;
mod config;
mod fork;
#[cfg(feature = "context-future")]
mod context_future;
#[macro_use]
mod macros;
pub mod async_ctx;
pub mod sync_ctx;

// Re-export public types
pub use error::ContextError;
pub use scope::ScopeGuard;
pub use snapshot::ContextSnapshot;

#[cfg(feature = "context-key")]
pub use context_key::ContextKey;

// ── Registration ───────────────────────────────────────────────

pub use registry::{RegistryBuilder, RegistrationOptions,
                   initialize, try_initialize,
                   with_metadata, keys_with_metadata};

// Re-export free-standing registration functions for internal tests only.
#[cfg(test)]
pub(crate) use registry::{register, try_register, register_with, try_register_with,
                   register_local, try_register_local,
                   register_migration, try_register_migration};

// ── Scope management ───────────────────────────────────────────

pub use sync_storage::enter_named_scope;

/// Push a new scope. Dispatches to task-local if available, else thread-local.
pub fn enter_scope() -> ScopeGuard {
    if crate::async_ctx::current_depth().is_some() {
        crate::async_ctx::push_scope("")
    } else {
        sync_storage::enter_scope()
    }
}

/// Execute `f` in a new scope. Changes revert when `f` returns.
/// Dispatches to task-local if available, else thread-local.
pub fn scope<R>(f: impl FnOnce() -> R) -> R {
    let _guard = enter_scope();
    f()
}

#[allow(deprecated)]
pub use sync_storage::{force_thread_local, scope_async, named_scope_async};

/// Get the current scope chain, dispatching to task-local first, then thread-local.
pub fn scope_chain() -> Vec<String> {
    let async_chain = crate::async_ctx::scope_chain();
    if !async_chain.is_empty() {
        return async_chain;
    }
    sync_storage::scope_chain()
}

// ── Snapshot / Clone ───────────────────────────────────────────

pub use snapshot::{snapshot, attach, wrap_with_context, wrap_with_context_fn};

// ── Fork (lightweight local spawn) ────────────────────────────

pub use fork::{ForkHandle, fork, with_fork, spawn_with_fork_async};

// ── Thread helpers ─────────────────────────────────────────────

pub use helpers::spawn_with_context;

// ── Async helpers (feature-gated) ──────────────────────────────

pub use helpers::{with_context, spawn_with_context_async};

// ── Runtime-agnostic async (feature-gated) ─────────────────────

#[cfg(feature = "context-future")]
pub use context_future::{ContextFuture, with_context_future};

// ── Serialization ──────────────────────────────────────────────

pub use wire::{serialize_context, deserialize_context, make_wire_bytes, make_wire_bytes_v};

#[cfg(feature = "base64")]
pub use wire::{serialize_context_string, deserialize_context_string};

// ── Configuration ──────────────────────────────────────────────

pub use config::{set_max_context_size, max_context_size,
                 set_max_scope_chain_len, max_scope_chain_len};

// ── Core get/set API ───────────────────────────────────────────
//
// The top-level API dispatches: tries task-local first, then thread-local.
// This provides backward compatibility. For explicit control, use
// `sync_ctx::*` (thread-local only) or `async_ctx::*` (task-local only).

use std::any::TypeId;
use sync_storage as storage;

/// Internal helper: get a value, trying task-local first, then thread-local.
fn dispatched_get_value(key: &str) -> Option<Arc<dyn value::ContextValue>> {
    // Try task-local first
    if let Some(val) = crate::async_ctx::get_raw_value(key) {
        return Some(val);
    }
    // Fall back to thread-local
    storage::get_value(key)
}

/// Internal helper: set a value, trying task-local first, then thread-local.
fn dispatched_set_value(key: &'static str, value: Arc<dyn value::ContextValue>) {
    // If task-local is available, write there
    if crate::async_ctx::current_depth().is_some() {
        crate::async_ctx::set_raw_value(key, value);
    } else {
        storage::set_value(key, value);
    }
}

use std::sync::Arc;

/// Get a context value. Returns `T::default()` if not set.
/// Panics if the key is not registered.
pub fn get_context<T>(key: &'static str) -> T
where
    T: Clone + Default + Send + Sync + 'static,
{
    try_get_context::<T>(key)
        .expect("dcontext::get_context: key not registered")
        .unwrap_or_default()
}

/// Get a context value as `Option<T>`. Returns `None` if the key is not set.
/// Panics if the key is not registered.
pub fn get_context_option<T>(key: &'static str) -> Option<T>
where
    T: Clone + Default + Send + Sync + 'static,
{
    try_get_context::<T>(key)
        .expect("dcontext::get_context_option: key not registered")
}

/// Get a context value. Returns `Ok(Some(T))` if set, `Ok(None)` if registered
/// but not set, `Err` if not registered.
pub fn try_get_context<T>(key: &'static str) -> Result<Option<T>, ContextError>
where
    T: Clone + Default + Send + Sync + 'static,
{
    // Hot path: try to get the value from storage first (no registry lock).
    let arc = dispatched_get_value(key);
    match arc {
        Some(val) => {
            let any_ref = val.as_any();
            match any_ref.downcast_ref::<T>() {
                Some(typed) => Ok(Some(typed.clone())),
                None => {
                    // Type mismatch — get names from registry for the error message.
                    let registered_name =
                        registry::with_registration(key, |r| r.type_name).unwrap_or("unknown");
                    Err(ContextError::TypeMismatch(
                        key.to_string(),
                        registered_name.to_string(),
                        std::any::type_name::<T>().to_string(),
                    ))
                }
            }
        }
        None => {
            // Value not in storage. Single registry lookup to distinguish
            // "registered but not set" from "not registered", and verify type.
            match registry::with_registration(key, |r| (r.type_id, r.type_name)) {
                None => Err(ContextError::NotRegistered(key.to_string())),
                Some((tid, type_name)) if tid != TypeId::of::<T>() => {
                    Err(ContextError::TypeMismatch(
                        key.to_string(),
                        type_name.to_string(),
                        std::any::type_name::<T>().to_string(),
                    ))
                }
                Some(_) => Ok(None),
            }
        }
    }
}

/// Set a context value in the current scope.
/// Panics if the key is not registered or type doesn't match.
///
/// Note: `T` must implement `Serialize + DeserializeOwned` because context values
/// must be serializable for cross-process propagation. This is enforced by the
/// `ContextValue` trait's blanket implementation.
pub fn set_context<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_set_context(key, value).expect("dcontext::set_context failed");
}

/// Set a context value. Returns Err on type mismatch or if key is not registered.
pub fn try_set_context<T>(key: &'static str, value: T) -> Result<(), ContextError>
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    match registry::with_registration(key, |r| (r.type_id, r.type_name)) {
        None => return Err(ContextError::NotRegistered(key.to_string())),
        Some((tid, type_name)) if tid != TypeId::of::<T>() => {
            return Err(ContextError::TypeMismatch(
                key.to_string(),
                type_name.to_string(),
                std::any::type_name::<T>().to_string(),
            ));
        }
        Some(_) => {}
    }

    dispatched_set_value(key, std::sync::Arc::new(value));
    Ok(())
}

/// Set a local-only context value. The type does NOT need Serialize/DeserializeOwned.
/// Panics if the key is not registered or type doesn't match.
pub fn set_context_local<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + 'static,
{
    try_set_context_local(key, value).expect("dcontext::set_context_local failed");
}

/// Set a local-only context value. Returns Err on type mismatch or if key is not registered.
pub fn try_set_context_local<T>(key: &'static str, value: T) -> Result<(), ContextError>
where
    T: Clone + Send + Sync + 'static,
{
    match registry::with_registration(key, |r| (r.type_id, r.type_name)) {
        None => return Err(ContextError::NotRegistered(key.to_string())),
        Some((tid, type_name)) if tid != TypeId::of::<T>() => {
            return Err(ContextError::TypeMismatch(
                key.to_string(),
                type_name.to_string(),
                std::any::type_name::<T>().to_string(),
            ));
        }
        Some(_) => {}
    }

    dispatched_set_value(key, std::sync::Arc::new(crate::value::LocalValue(value)));
    Ok(())
}

// ── Type-erased value access (for extension crates) ────────────

/// Access the current context value for a key as `&dyn Any` via callback.
///
/// This is the low-level hook for extension crates (like dcontext-tracing)
/// that need type-erased access to context values — e.g., for formatting
/// values into log output without knowing the concrete type at the call site.
///
/// Returns `None` if the key has no value set or the store is busy.
/// Never panics.
pub fn with_context_value<R>(
    key: &str,
    f: impl FnOnce(&dyn std::any::Any) -> R,
) -> Option<R> {
    dispatched_get_value(key).map(|arc_val| f(arc_val.as_any()))
}

// ── Update API (read-modify-write) ─────────────────────────────

/// Update a context value using a callback. Reads the current value,
/// passes it to `f`, and writes the result back.
///
/// This is **not atomic**: another write may interleave between the read
/// and the write. Last writer wins — this is by design for contention-free
/// access. The callback runs with the store fully available (no "busy"
/// window), so re-entrant reads from tracing callbacks etc. work normally.
///
/// Panics if the key is not registered or type doesn't match.
///
/// # Example
///
/// ```rust,ignore
/// update_context::<Counter>("counter", |c| Counter(c.0 + 1));
/// ```
pub fn update_context<T>(key: &'static str, f: impl FnOnce(T) -> T)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    try_update_context::<T>(key, f).expect("dcontext::update_context failed");
}

/// Update a context value using a callback. Returns Err on type mismatch
/// or if the key is not registered.
///
/// See [`update_context`] for details on semantics and concurrency.
pub fn try_update_context<T>(key: &'static str, f: impl FnOnce(T) -> T) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    // Step 1: Read current value (brief store window — just Arc::clone).
    let old = try_get_context::<T>(key)?.unwrap_or_default();

    // Step 2: User callback — store is fully available.
    let new = f(old);

    // Step 3: Write new value (brief store window — just pointer swap).
    try_set_context(key, new)
}

/// Update a local-only context value using a callback.
/// Panics if the key is not registered or type doesn't match.
pub fn update_context_local<T>(key: &'static str, f: impl FnOnce(T) -> T)
where
    T: Clone + Default + Send + Sync + 'static,
{
    try_update_context_local::<T>(key, f).expect("dcontext::update_context_local failed");
}

/// Update a local-only context value using a callback. Returns Err on type
/// mismatch or if the key is not registered.
pub fn try_update_context_local<T>(key: &'static str, f: impl FnOnce(T) -> T) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + 'static,
{
    let old = try_get_context::<T>(key)?.unwrap_or_default();
    let new = f(old);
    try_set_context_local(key, new)
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests;

