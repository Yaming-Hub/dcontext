//! Async (task-local) storage for dcontext.
//!
//! Contains the `TASK_CONTEXT` task-local variable used by `async_ctx` and
//! the dispatch layer to provide per-task context isolation.

use std::cell::Cell;

use crate::sync_storage::ContextStore;

tokio::task_local! {
    /// Task-local context store. Each async task gets its own isolated store.
    /// Accessed via `async_ctx` module functions or the dispatch in `sync_storage`.
    pub(crate) static TASK_CONTEXT: Cell<Option<ContextStore>>;
}
