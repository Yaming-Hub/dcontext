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
//! use dcontext::{RegistryBuilder, initialize};
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Clone, Default, Debug, Serialize, Deserialize)]
//! struct RequestId(String);
//!
//! # fn main() {
//! let mut builder = RegistryBuilder::new();
//! builder.register::<RequestId>("request_id");
//! initialize(builder);
//!
//! let _guard = dcontext::sync_ctx::push_scope("ingress");
//! dcontext::sync_ctx::set_context("request_id", RequestId("req-123".into()));
//!
//! let rid: Option<RequestId> = dcontext::sync_ctx::get_context("request_id");
//! assert_eq!(rid.unwrap().0, "req-123");
//!
//! let chain = dcontext::sync_ctx::scope_chain();
//! assert_eq!(chain, vec!["ingress"]);
//! # }
//! ```
//!
//! Use [`sync_ctx`] for thread-local context and [`async_ctx`] for
//! task-local context. See [`sync_ctx::serialize_context`] and
//! [`async_ctx::serialize_context`] for cross-process propagation.

pub mod error;
mod registry;
mod scope;
mod snapshot;
pub(crate) mod store;
pub mod value;
mod wire;

mod config;
#[cfg(feature = "context-key")]
mod context_key;
mod inheritance;
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

pub use registry::{
    initialize, keys_with_metadata, try_initialize, with_metadata, RegistrationOptions,
    RegistryBuilder,
};

// Re-export free-standing registration functions for internal tests only.
#[cfg(test)]
pub(crate) use registry::{
    register, register_local, register_migration, register_with, try_register, try_register_local,
    try_register_migration, try_register_with,
};

// ── Scope management ───────────────────────────────────────────

pub use sync_ctx::{enter_named_scope, enter_scope};

// ── Context inheritance (spawn helpers) ───────────────────────

pub use inheritance::{
    spawn_blocking_with_async_context, spawn_with_async_context, spawn_with_sync_context,
    wrap_with_async_context, wrap_with_sync_context, ContextInheritance,
};

// ── Serialization (helpers) ───────────────────────────────────

pub use wire::{deserialize_to_snapshot, make_wire_bytes, make_wire_bytes_v};

// ── Configuration ──────────────────────────────────────────────

pub use config::{
    max_context_size, max_scope_chain_len, set_max_context_size, set_max_scope_chain_len,
};

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
