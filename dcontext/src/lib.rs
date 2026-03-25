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
//! use dcontext::{register, enter_scope, get_context, set_context};
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Clone, Default, Debug, Serialize, Deserialize)]
//! struct RequestId(String);
//!
//! # fn main() {
//! register::<RequestId>("request_id");
//!
//! let _guard = enter_scope();
//! set_context("request_id", RequestId("req-123".into()));
//!
//! let rid: RequestId = get_context("request_id");
//! assert_eq!(rid.0, "req-123");
//! # }
//! ```

pub mod error;
mod value;
mod registry;
mod scope;
mod storage;
mod snapshot;
mod wire;
mod helpers;
#[cfg(feature = "context-key")]
mod context_key;
mod config;
#[cfg(feature = "context-future")]
mod context_future;
#[macro_use]
mod macros;

// Re-export public types
pub use error::ContextError;
pub use scope::ScopeGuard;
pub use snapshot::ContextSnapshot;

#[cfg(feature = "context-key")]
pub use context_key::ContextKey;

// ── Registration ───────────────────────────────────────────────

pub use registry::{register, try_register, register_with, try_register_with,
                   register_local, try_register_local,
                   register_migration, try_register_migration,
                   RegistrationOptions};

// ── Scope management ───────────────────────────────────────────

pub use storage::{enter_scope, scope, force_thread_local};

#[cfg(feature = "tokio")]
pub use storage::scope_async;

// ── Snapshot / Clone ───────────────────────────────────────────

pub use snapshot::{snapshot, attach, wrap_with_context, wrap_with_context_fn};

// ── Thread helpers ─────────────────────────────────────────────

pub use helpers::spawn_with_context;

// ── Async helpers (feature-gated) ──────────────────────────────

#[cfg(feature = "tokio")]
pub use helpers::{with_context, spawn_with_context_async};

// ── Runtime-agnostic async (feature-gated) ─────────────────────

#[cfg(feature = "context-future")]
pub use context_future::{ContextFuture, with_context_future};

// ── Serialization ──────────────────────────────────────────────

pub use wire::{serialize_context, deserialize_context, make_wire_bytes};

#[cfg(feature = "base64")]
pub use wire::{serialize_context_string, deserialize_context_string};

// ── Configuration ──────────────────────────────────────────────

pub use config::{set_max_context_size, max_context_size};

// ── Core get/set API ───────────────────────────────────────────

use std::any::TypeId;

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

/// Get a context value. Returns `Ok(Some(T))` if set, `Ok(None)` if registered
/// but not set, `Err` if not registered.
pub fn try_get_context<T>(key: &'static str) -> Result<Option<T>, ContextError>
where
    T: Clone + Default + Send + Sync + 'static,
{
    // Hot path: try to get the value from storage first (no registry lock).
    let boxed = storage::get_value(key);
    match boxed {
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

    storage::set_value(key, Box::new(value));
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

    storage::set_value(key, Box::new(crate::value::LocalValue(value)));
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests;

