use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::scope::ScopeNode;
use crate::async_ctx::TASK_CONTEXT;
use crate::sync_ctx::{self as storage};
use crate::store::ContextStore;
use crate::value::ContextValue;

/// A lightweight, shareable handle to a parent context.
///
/// Created by [`fork()`], this handle captures the current context state
/// via `Arc` sharing — no value cloning. Use with [`with_fork()`] or
/// [`spawn_with_fork()`] to establish the context in a child task.
///
/// ## Cost
///
/// Creating a `ForkHandle` costs approximately one `Arc::clone` per active
/// context key (just atomic ref-count bumps, no data copying). This is
/// significantly cheaper than [`snapshot()`](crate::snapshot) which walks
/// the full scope chain and clones all effective values.
///
/// ## Semantics
///
/// - **Reads** in the child see the parent's values (shared via Arc).
/// - **Writes** in the child create a local overlay (copy-on-write);
///   the parent is never affected.
/// - The handle is `Send + Sync` — safe to move across task boundaries.
#[derive(Clone)]
pub struct ForkHandle {
    /// The frozen parent scope state. None if forked from an empty/busy context.
    frozen: Option<Arc<ScopeNode>>,
    /// Remote chain at time of fork.
    remote_chain: Arc<Vec<String>>,
    /// Remote chain base depth at time of fork.
    remote_chain_base_depth: usize,
    /// Cached keys pre-extracted from the frozen state for O(1) reads in child.
    cached_values: HashMap<&'static str, Arc<dyn ContextValue>>,
}

impl ForkHandle {
    /// Create an empty handle (fallback when store is busy).
    fn empty() -> Self {
        Self {
            frozen: None,
            remote_chain: Arc::new(Vec::new()),
            remote_chain_base_depth: 0,
            cached_values: HashMap::new(),
        }
    }
}

/// Create a lightweight handle to the current context for spawning child tasks.
///
/// Unlike [`snapshot()`](crate::snapshot), this does **not** walk the full scope
/// chain or clone values. It captures the current scope state via Arc sharing.
///
/// # Example
///
/// ```rust,ignore
/// let handle = dcontext::fork();
/// tokio::spawn(dcontext::with_fork(handle, async {
///     // reads see parent's context values
///     let rid: String = dcontext::get_context("request_id");
/// }));
/// ```
pub fn fork() -> ForkHandle {
    storage::do_fork().unwrap_or_else(ForkHandle::empty)
}

/// Run an async future with a forked context as the active task-local context.
///
/// The child task starts with a read-only view of the parent's scope.
/// Writes create a local overlay (COW) — the parent is unaffected.
///
/// # Example
///
/// ```rust,ignore
/// let handle = dcontext::fork();
/// tokio::spawn(dcontext::with_fork(handle, async {
///     // reads work, writes are isolated
///     dcontext::set_context("key", new_value);  // only visible in this task
/// }));
/// ```
pub async fn with_fork<F>(handle: ForkHandle, future: F) -> F::Output
where
    F: std::future::Future,
{
    let store = build_forked_store(handle);
    TASK_CONTEXT.scope(Cell::new(Some(store)), future).await
}

/// Spawn a Tokio task that inherits the current context via fork (cheap).
///
/// This is the recommended replacement for [`spawn_with_context_async`](crate::spawn_with_context_async)
/// when the child task only needs **read access** to the parent's context values
/// (the common case for local spawns).
///
/// # Example
///
/// ```rust,ignore
/// dcontext::spawn_with_fork_async(async {
///     let rid: String = dcontext::get_context("request_id");
///     // ... use context values
/// });
/// ```
pub fn spawn_with_fork_async<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handle = fork();
    tokio::spawn(with_fork(handle, future))
}

/// Build a ContextStore for the child from a ForkHandle.
fn build_forked_store(handle: ForkHandle) -> ContextStore {
    let (depth, remote_chain, remote_chain_base_depth) = match &handle.frozen {
        Some(node) => (
            node.depth + 1,
            Arc::clone(&handle.remote_chain),
            handle.remote_chain_base_depth,
        ),
        None => (1, handle.remote_chain, handle.remote_chain_base_depth),
    };

    ContextStore {
        scope_chain: handle.frozen,
        current_values: handle.cached_values,
        current_name: None,
        depth,
        remote_chain,
        remote_chain_base_depth,
    }
}

// ── Internal: called from storage.rs ───────────────────────────

/// Create a ForkHandle from the current store state.
/// Called within the Cell window via with_context.
pub(crate) fn create_fork_handle(store: &ContextStore) -> ForkHandle {
    // Freeze the current scope into a new ScopeNode (Arc-shared with parent)
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

    // Pre-populate cached keys for O(1) reads in child
    let cached_keys = crate::registry::cached_keys();
    let mut cached_values: HashMap<&'static str, Arc<dyn ContextValue>> = HashMap::new();
    for key in cached_keys {
        if let Some(val) = store.get_value(key) {
            cached_values.insert(key, val);
        }
    }

    ForkHandle {
        remote_chain: Arc::clone(&store.remote_chain),
        remote_chain_base_depth: store.remote_chain_base_depth,
        frozen: Some(frozen),
        cached_values,
    }
}
