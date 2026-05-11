//! Re-exports from `sync_storage` and `async_storage` for backward compatibility.
//!
//! All storage functionality has been split into:
//! - `sync_storage.rs` -- thread-local storage, ContextStore, dispatch logic
//! - `async_storage.rs` -- task-local storage (TASK_CONTEXT)

pub(crate) use crate::async_storage::TASK_CONTEXT;
pub(crate) use crate::sync_storage::{
    ContextStore, CONTEXT,
    leave_scope, do_fork,
    get_value, set_value, collect_values, collect_scope_chain,
    set_remote_chain,
};

// Public API re-exports (visible outside the crate via lib.rs)
pub use crate::sync_storage::{
    enter_scope, enter_named_scope, scope, scope_chain, force_thread_local,
    scope_async, named_scope_async,
};
