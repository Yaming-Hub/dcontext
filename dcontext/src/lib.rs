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

// Re-export public types
pub use error::ContextError;
pub use scope::ScopeGuard;
pub use snapshot::ContextSnapshot;

// ── Registration ───────────────────────────────────────────────

pub use registry::{register, try_register};

// ── Scope management ───────────────────────────────────────────

pub use storage::{enter_scope, scope, force_thread_local};

// ── Snapshot / Clone ───────────────────────────────────────────

pub use snapshot::{snapshot, attach, wrap_with_context, wrap_with_context_fn};

// ── Thread helpers ─────────────────────────────────────────────

pub use helpers::spawn_with_context;

// ── Async helpers (feature-gated) ──────────────────────────────

#[cfg(feature = "tokio")]
pub use helpers::{with_context, spawn_with_context_async};

// ── Serialization ──────────────────────────────────────────────

pub use wire::{serialize_context, deserialize_context};

#[cfg(feature = "base64")]
pub use wire::{serialize_context_string, deserialize_context_string};

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
            // Value not in storage. Check registry (cold path) to distinguish
            // "registered but not set" from "not registered".
            if !registry::is_registered(key) {
                return Err(ContextError::NotRegistered(key.to_string()));
            }
            // Verify type matches even when returning None.
            let expected_tid = TypeId::of::<T>();
            let registered_tid = registry::type_id_for(key).unwrap();
            if expected_tid != registered_tid {
                let registered_name =
                    registry::with_registration(key, |r| r.type_name).unwrap_or("unknown");
                return Err(ContextError::TypeMismatch(
                    key.to_string(),
                    registered_name.to_string(),
                    std::any::type_name::<T>().to_string(),
                ));
            }
            Ok(None)
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
    if !registry::is_registered(key) {
        return Err(ContextError::NotRegistered(key.to_string()));
    }

    let expected_tid = registry::type_id_for(key).unwrap();
    if TypeId::of::<T>() != expected_tid {
        let expected_name =
            registry::with_registration(key, |r| r.type_name).unwrap_or("unknown");
        return Err(ContextError::TypeMismatch(
            key.to_string(),
            expected_name.to_string(),
            std::any::type_name::<T>().to_string(),
        ));
    }

    storage::set_value(key, Box::new(value));
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests;

