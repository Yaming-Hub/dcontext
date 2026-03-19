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

// Modules will be added during implementation:
// mod registry;
// mod scope;
// mod storage;
// mod snapshot;
// mod error;
// mod helpers;
// mod macros;
