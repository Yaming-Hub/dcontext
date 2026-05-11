//! Fork — lightweight context inheritance via Arc sharing.
//!
//! Fork creates a new branch from the current scope node. The child starts
//! with a fresh root scope whose `frozen_parent` points back to the parent's
//! scope chain. Value lookups in the child fall through to the frozen parent,
//! so reads see parent values without copying. Writes go to the child's own
//! scope (copy-on-write semantics).
//!
//! Three spawn helpers cover the common cross-boundary scenarios:
//!
//! | Helper | Source | Target | Use case |
//! |--------|--------|--------|----------|
//! | [`spawn_fork_async_context`] | async (task-local) | async task | `tokio::spawn` from async |
//! | [`spawn_blocking_fork_async_context`] | async (task-local) | blocking thread | `spawn_blocking` from async |
//! | [`spawn_fork_sync_context`] | sync (thread-local) | async task | `tokio::spawn` from sync |

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::scope::ScopeNode;
use crate::async_ctx::{self, TASK_CONTEXT};
use crate::sync_ctx;
use crate::store::ContextStore;
use crate::value::ContextValue;

/// A lightweight, shareable handle to a parent context.
///
/// Captures the current scope state via `Arc` sharing — no value cloning.
/// The handle is `Send + Sync` and safe to move across task/thread boundaries.
///
/// ## Cost
///
/// Creating a `ForkHandle` costs one `Arc::clone` per key in the current
/// scope plus one `Arc::clone` for the parent chain pointer — just atomic
/// ref-count bumps, no data copying.
///
/// ## Semantics
///
/// - **Reads** in the child see the parent's values (shared via Arc).
/// - **Writes** in the child create a local overlay (copy-on-write);
///   the parent is never affected.
#[derive(Clone)]
pub struct ForkHandle {
    /// The frozen parent scope node. None if forked from an empty/busy context.
    frozen: Option<Arc<ScopeNode>>,
    /// Remote chain at time of fork.
    remote_chain: Arc<Vec<String>>,
    /// Remote chain base depth at time of fork.
    remote_chain_base_depth: usize,
}

impl ForkHandle {
    /// Create an empty handle (fallback when store is busy).
    fn empty() -> Self {
        Self {
            frozen: None,
            remote_chain: Arc::new(Vec::new()),
            remote_chain_base_depth: 0,
        }
    }
}

// ── Scenario 1: async task → spawn async task ─────────────────

/// Spawn a Tokio task that inherits the current **async** (task-local)
/// context via fork.
///
/// The child task gets a fresh root scope with the parent's scope as its
/// frozen parent. Reads see parent values; writes are isolated.
///
/// # Example
///
/// ```rust,ignore
/// // Inside an async task with active task-local context
/// dcontext::spawn_fork_async_context(async {
///     // reads see parent's values
///     let rid: String = dcontext::async_ctx::get_context("request_id").unwrap();
/// });
/// ```
pub fn spawn_fork_async_context<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handle = async_ctx::do_fork().unwrap_or_else(ForkHandle::empty);
    let store = build_child_store(handle);
    tokio::spawn(TASK_CONTEXT.scope(Cell::new(Some(store)), future))
}

// ── Scenario 2: async task → spawn_blocking thread ────────────

/// Spawn a blocking thread that inherits the current **async** (task-local)
/// context via fork.
///
/// Forks from the task-local context and installs it into the blocking
/// thread's thread-local context. The thread-local store is restored
/// when the closure returns.
///
/// # Example
///
/// ```rust,ignore
/// // Inside an async task with active task-local context
/// dcontext::spawn_blocking_fork_async_context(|| {
///     // reads see parent's values via thread-local
///     let rid: String = dcontext::sync_ctx::get_context("request_id").unwrap();
/// });
/// ```
pub fn spawn_blocking_fork_async_context<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let handle = async_ctx::do_fork().unwrap_or_else(ForkHandle::empty);
    let store = build_child_store(handle);
    tokio::task::spawn_blocking(move || {
        // Swap the forked context into this thread's thread-local store.
        // Restore the original on exit so thread-pool threads stay clean.
        let saved = sync_ctx::try_apply(|s| std::mem::replace(s, store));
        let result = f();
        if let Some(original) = saved {
            sync_ctx::try_apply(|s| *s = original);
        }
        result
    })
}

// ── Scenario 3: sync code → spawn async task ─────────────────

/// Spawn a Tokio task that inherits the current **sync** (thread-local)
/// context via fork.
///
/// Forks from the thread-local context and installs it into the child
/// task's task-local context. Use this when spawning async work from
/// synchronous code.
///
/// # Example
///
/// ```rust,ignore
/// // In sync code with active thread-local context
/// dcontext::spawn_fork_sync_context(async {
///     // reads see parent's values via task-local
///     let rid: String = dcontext::async_ctx::get_context("request_id").unwrap();
/// });
/// ```
pub fn spawn_fork_sync_context<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handle = sync_ctx::do_fork().unwrap_or_else(ForkHandle::empty);
    let store = build_child_store(handle);
    tokio::spawn(TASK_CONTEXT.scope(Cell::new(Some(store)), future))
}

// ── Deprecated compatibility ──────────────────────────────────

/// Create a fork handle from the current sync context.
///
/// **Deprecated**: Use [`spawn_fork_sync_context`], [`spawn_fork_async_context`],
/// or [`spawn_blocking_fork_async_context`] instead.
#[deprecated(note = "Use spawn_fork_sync_context, spawn_fork_async_context, or spawn_blocking_fork_async_context")]
pub fn fork() -> ForkHandle {
    sync_ctx::do_fork().unwrap_or_else(ForkHandle::empty)
}

/// Run a future with a forked context as the active task-local context.
///
/// **Deprecated**: Use [`spawn_fork_async_context`] or [`spawn_fork_sync_context`] instead.
#[deprecated(note = "Use spawn_fork_async_context or spawn_fork_sync_context")]
pub async fn with_fork<F>(handle: ForkHandle, future: F) -> F::Output
where
    F: std::future::Future,
{
    let store = build_child_store(handle);
    TASK_CONTEXT.scope(Cell::new(Some(store)), future).await
}

/// Spawn a Tokio task with forked sync context.
///
/// **Deprecated**: Use [`spawn_fork_sync_context`] instead.
#[deprecated(note = "Use spawn_fork_sync_context")]
pub fn spawn_with_fork_async<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn_fork_sync_context(future)
}

// ── Internal ──────────────────────────────────────────────────

/// Build a child ContextStore from a ForkHandle.
///
/// The child starts with a fresh root scope (depth=1). If the handle
/// has a frozen parent, value lookups fall through to it automatically
/// via `ContextStore::frozen_parent`.
fn build_child_store(handle: ForkHandle) -> ContextStore {
    match handle.frozen {
        Some(parent) => ContextStore::from_fork(
            parent,
            handle.remote_chain,
            handle.remote_chain_base_depth,
        ),
        None => ContextStore::new(),
    }
}

/// Create a ForkHandle from the current store state.
/// Called within the Cell window via try_apply.
pub(crate) fn create_fork_handle(store: &ContextStore) -> ForkHandle {
    // Freeze the current scope into a new ScopeNode (Arc-shared with parent).
    let frozen_values: HashMap<&'static str, Arc<dyn ContextValue>> = store
        .current_values
        .iter()
        .map(|(&k, v)| (k, Arc::clone(v)))
        .collect();

    let frozen = Arc::new(ScopeNode {
        name: store.current_name.clone(),
        values: frozen_values,
        parent: store.scope_chain.clone(),
        depth: store.depth,
        remote_chain: Arc::clone(&store.remote_chain),
        remote_chain_base_depth: store.remote_chain_base_depth,
    });

    ForkHandle {
        remote_chain: Arc::clone(&store.remote_chain),
        remote_chain_base_depth: store.remote_chain_base_depth,
        frozen: Some(frozen),
    }
}
