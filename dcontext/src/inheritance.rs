//! Context inheritance — propagating context across task/thread boundaries.
//!
//! Three spawn helpers cover the common cross-boundary scenarios:
//!
//! | Helper | Source | Target |
//! |--------|--------|--------|
//! | [`spawn_with_async_context`] | async (task-local) | async task |
//! | [`spawn_blocking_with_async_context`] | async (task-local) | blocking thread |
//! | [`spawn_with_sync_context`] | sync (thread-local) | async task |
//!
//! Each takes a [`ContextInheritance`] mode:
//!
//! - **Fork** — cheap Arc sharing. Child sees parent's values at fork time.
//!   Reads fall through to the frozen parent; writes are isolated (COW).
//! - **Snapshot** — full copy. Walks the entire scope chain, clones all
//!   effective values into a flat HashMap. O(1) reads in the child.

use std::cell::Cell;
use std::sync::Arc;

use crate::async_ctx::{self, TASK_CONTEXT};
use crate::sync_ctx;
use crate::store::ContextStore;
use crate::snapshot::ContextSnapshot;

/// Controls how context is captured when crossing task/thread boundaries.
///
/// - **Fork** — lightweight Arc sharing. The child's scope inherits from
///   a frozen parent node. Reads fall through; writes are isolated (COW).
///   Cheapest option when the child mostly reads.
/// - **Snapshot** — full deep copy. All effective values are collected into
///   a flat HashMap. More expensive to create but O(1) reads in the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextInheritance {
    /// Lightweight Arc sharing — O(current_scope_keys) to create.
    #[default]
    Fork,
    /// Full value copy — O(depth × keys) to create, O(1) reads.
    Snapshot,
}

// ── Spawn helpers ─────────────────────────────────────────────

/// Spawn a Tokio task that inherits the current **async** (task-local) context.
///
/// # Example
///
/// ```rust,ignore
/// use dcontext::{spawn_with_async_context, ContextInheritance};
///
/// // Fork (default, cheapest)
/// spawn_with_async_context(ContextInheritance::Fork, async {
///     let rid: String = dcontext::async_ctx::get_context("request_id").unwrap();
/// });
///
/// // Snapshot (full copy)
/// spawn_with_async_context(ContextInheritance::Snapshot, async { /* ... */ });
/// ```
pub fn spawn_with_async_context<F>(
    mode: ContextInheritance,
    future: F,
) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let store = capture_async(mode);
    tokio::spawn(TASK_CONTEXT.scope(Cell::new(Some(store)), future))
}

/// Spawn a blocking thread that inherits the current **async** (task-local) context.
///
/// The inherited context is installed into the blocking thread's thread-local
/// store and restored when the closure returns.
///
/// # Example
///
/// ```rust,ignore
/// use dcontext::{spawn_blocking_with_async_context, ContextInheritance};
///
/// spawn_blocking_with_async_context(ContextInheritance::Fork, || {
///     let rid: String = dcontext::sync_ctx::get_context("request_id").unwrap();
/// });
/// ```
pub fn spawn_blocking_with_async_context<F, R>(
    mode: ContextInheritance,
    f: F,
) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let store = capture_async(mode);
    tokio::task::spawn_blocking(move || {
        // Swap into this thread's thread-local store; restore on exit.
        let saved = sync_ctx::try_apply(|s| std::mem::replace(s, store));
        let result = f();
        if let Some(original) = saved {
            sync_ctx::try_apply(|s| *s = original);
        }
        result
    })
}

/// Spawn a Tokio task that inherits the current **sync** (thread-local) context.
///
/// # Example
///
/// ```rust,ignore
/// use dcontext::{spawn_with_sync_context, ContextInheritance};
///
/// spawn_with_sync_context(ContextInheritance::Fork, async {
///     let rid: String = dcontext::async_ctx::get_context("request_id").unwrap();
/// });
/// ```
pub fn spawn_with_sync_context<F>(
    mode: ContextInheritance,
    future: F,
) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let store = capture_sync(mode);
    tokio::spawn(TASK_CONTEXT.scope(Cell::new(Some(store)), future))
}

// ── Internal: capture context into a ContextStore ─────────────

fn capture_async(mode: ContextInheritance) -> ContextStore {
    match mode {
        ContextInheritance::Fork => {
            async_ctx::fork().unwrap_or_else(ContextStore::new)
        }
        ContextInheritance::Snapshot => {
            snapshot_to_store(async_ctx::snapshot())
        }
    }
}

fn capture_sync(mode: ContextInheritance) -> ContextStore {
    match mode {
        ContextInheritance::Fork => {
            sync_ctx::fork().unwrap_or_else(ContextStore::new)
        }
        ContextInheritance::Snapshot => {
            snapshot_to_store(sync_ctx::snapshot())
        }
    }
}

fn snapshot_to_store(snap: ContextSnapshot) -> ContextStore {
    let chain = snap.scope_chain.clone();
    let values = snap
        .values
        .iter()
        .map(|(k, v)| (*k, Arc::clone(v)))
        .collect();
    ContextStore::from_values_with_chain(values, chain)
}
