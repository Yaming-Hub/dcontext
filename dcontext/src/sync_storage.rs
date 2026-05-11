//! Sync (thread-local) storage for dcontext.
//!
//! Contains `ContextStore` (the shared store type), the thread-local `CONTEXT`,
//! and all thread-local-based public API functions (scope management, value access, etc.).
//!
//! With the dual-context redesign, there is no dispatch logic — this module
//! always uses the thread-local store. For async (task-local) context, use
//! `async_ctx` / `async_storage` directly.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::scope::{ScopeGarbage, ScopeGuard, ScopeNode};
use crate::value::ContextValue;

// ── Thread-local storage ───────────────────────────────────────
//
// The store lives in `Cell<Option<ContextStore>>`.
// `Some(store)` = normal state.
// `None`        = "busy" — store was taken for modification.
//
// Thread-local is eagerly initialized to `Some(ContextStore::new())`,
// so `None` always means "busy" (never "uninitialized").

thread_local! {
    pub(crate) static CONTEXT: Cell<Option<ContextStore>> =
        Cell::new(Some(ContextStore::new()));
}

// ── Mutable context store ──────────────────────────────────────

/// The active context state. Lives in `Cell<Option<ContextStore>>`.
///
/// ## Contention-free design
///
/// The store is accessed via the `Cell` take/set pattern:
/// 1. `cell.take()` — move store out (Cell becomes `None` = "busy")
/// 2. Modify store — only refcount bumps and pointer moves, no user code
/// 3. `cell.set(Some(store))` — move store back
///
/// User code (Clone, Drop, callbacks) runs **after** step 3, when the
/// Cell holds a valid store. Re-entrant access during user code succeeds.
/// Re-entrant access during steps 1–3 sees `None` and returns defaults.
pub(crate) struct ContextStore {
    /// Frozen parent scope chain (immutable linked list).
    pub(crate) scope_chain: Option<Arc<ScopeNode>>,
    /// Active scope's values (sparse — only keys set in this scope).
    /// For cached keys, the effective value is eagerly copied here on scope entry.
    /// For non-cached keys, only values explicitly set in this scope appear.
    pub(crate) current_values: HashMap<&'static str, Arc<dyn ContextValue>>,
    /// Name of the active scope (if named).
    pub(crate) current_name: Option<String>,
    /// Current scope depth (1 = root scope).
    /// Also serves as a unique identifier for each scope: depth increments on
    /// push and is never reused, so it uniquely identifies the active scope
    /// within this store instance.
    pub(crate) depth: usize,
    /// Remote scope chain (from cross-process propagation).
    pub(crate) remote_chain: Arc<Vec<String>>,
    /// Scope depth at which remote_chain was installed.
    /// Local names at depth > this value are included in scope_chain().
    pub(crate) remote_chain_base_depth: usize,
}

impl ContextStore {
    /// Create a new store with an empty root scope.
    pub(crate) fn new() -> Self {
        Self {
            scope_chain: None,
            current_values: HashMap::new(),
            current_name: None,
            depth: 1,
            remote_chain: Arc::new(Vec::new()),
            remote_chain_base_depth: 0,
        }
    }

    /// Build a store from a set of values and a remote scope chain.
    /// Used by snapshot attach and task-local initialization.
    pub(crate) fn from_values_with_chain(
        values: HashMap<&'static str, Arc<dyn ContextValue>>,
        remote_chain: Vec<String>,
    ) -> Self {
        Self {
            scope_chain: None,
            current_values: values,
            current_name: None,
            depth: 1,
            remote_chain: Arc::new(remote_chain),
            remote_chain_base_depth: 1,
        }
    }

    /// Freeze the current scope into an immutable ScopeNode and push it
    /// onto the scope chain. The new scope starts with only cached keys
    /// pre-populated (their effective values are Arc::cloned in).
    /// Non-cached keys are absent — reads walk the parent chain.
    ///
    /// Returns the new depth.
    pub(crate) fn push_scope(&mut self, name: Option<String>) -> usize {
        let cached = crate::registry::cached_keys();
        let mut cached_values: Vec<(&'static str, Arc<dyn ContextValue>)> = Vec::new();
        for &key in &cached {
            if let Some(val) = self.get_value(key) {
                cached_values.push((key, val));
            }
        }

        let frozen_values = std::mem::take(&mut self.current_values);

        let node = Arc::new(ScopeNode {
            name: self.current_name.take(),
            values: frozen_values,
            parent: self.scope_chain.take(),
            depth: self.depth,
            remote_chain: Arc::clone(&self.remote_chain),
            remote_chain_base_depth: self.remote_chain_base_depth,
        });

        self.scope_chain = Some(node);
        self.current_name = name;
        self.depth += 1;

        for (key, val) in cached_values {
            self.current_values.insert(key, val);
        }

        self.depth
    }

    /// Pop the current scope, restoring state from the frozen ScopeNode.
    ///
    /// Returns the garbage (old current_values) to be dropped OUTSIDE
    /// the Cell window.
    pub(crate) fn pop_scope(
        &mut self,
        expected_depth: usize,
    ) -> Option<ScopeGarbage> {
        if self.depth != expected_depth || self.depth <= 1 {
            return None;
        }

        let node = self.scope_chain.take()?;

        let old_current = std::mem::take(&mut self.current_values);

        match Arc::try_unwrap(node) {
            Ok(owned) => {
                self.scope_chain = owned.parent;
                self.current_name = owned.name;
                self.current_values = owned.values;
                self.depth = owned.depth;
                self.remote_chain = owned.remote_chain;
                self.remote_chain_base_depth = owned.remote_chain_base_depth;
            }
            Err(shared) => {
                self.scope_chain = shared.parent.clone();
                self.current_name = shared.name.clone();
                self.current_values = shared.values.iter()
                    .map(|(&k, v)| (k, Arc::clone(v)))
                    .collect();
                self.depth = shared.depth;
                self.remote_chain = Arc::clone(&shared.remote_chain);
                self.remote_chain_base_depth = shared.remote_chain_base_depth;
            }
        }

        Some(ScopeGarbage {
            _old_values: old_current,
        })
    }

    /// Set a value in the current scope.
    pub(crate) fn set_value(
        &mut self,
        key: &'static str,
        value: Arc<dyn ContextValue>,
    ) -> Option<Arc<dyn ContextValue>> {
        self.current_values.insert(key, value)
    }

    /// Look up the effective value for a key.
    /// Checks current_values first, then walks the parent scope chain.
    /// For cached keys, the value is always in current_values (O(1)).
    /// For non-cached keys, this is O(depth).
    pub(crate) fn get_value(&self, key: &str) -> Option<Arc<dyn ContextValue>> {
        if let Some(v) = self.current_values.get(key) {
            return Some(Arc::clone(v));
        }
        let mut node = self.scope_chain.as_ref();
        while let Some(n) = node {
            if let Some(v) = n.values.get(key) {
                return Some(Arc::clone(v));
            }
            node = n.parent.as_ref();
        }
        None
    }

    /// Collect all effective values for snapshot/serialization.
    /// Walks the full scope chain to build a merged view (topmost wins).
    pub(crate) fn collect_values(&self) -> HashMap<&'static str, Arc<dyn ContextValue>> {
        let mut result: HashMap<&'static str, Arc<dyn ContextValue>> = HashMap::new();

        for (&k, v) in &self.current_values {
            result.insert(k, Arc::clone(v));
        }

        let mut node = self.scope_chain.as_ref();
        while let Some(n) = node {
            for (&k, v) in &n.values {
                result.entry(k).or_insert_with(|| Arc::clone(v));
            }
            node = n.parent.as_ref();
        }

        result
    }

    /// Set the remote scope chain.
    pub(crate) fn set_remote_chain(&mut self, chain: Vec<String>) {
        self.remote_chain = Arc::new(chain);
        self.remote_chain_base_depth = self.depth;
    }

    /// Build the full scope chain: remote prefix + local named scope names.
    pub(crate) fn scope_chain(&self) -> Vec<String> {
        let mut local_names = Vec::new();

        if let Some(name) = &self.current_name {
            if self.depth > self.remote_chain_base_depth {
                local_names.push(name.clone());
            }
        }

        let mut node = self.scope_chain.as_ref();
        while let Some(n) = node {
            if n.depth > self.remote_chain_base_depth {
                if let Some(name) = &n.name {
                    local_names.push(name.clone());
                }
            }
            node = n.parent.as_ref();
        }

        local_names.reverse();

        let max_len = crate::config::max_scope_chain_len();
        let mut chain: Vec<String> = (*self.remote_chain).clone();
        chain.extend(local_names);

        if max_len > 0 && chain.len() > max_len {
            let start = chain.len() - max_len;
            chain.drain(..start);
        }

        chain
    }
}

// ── Cell access ────────────────────────────────────────────────

/// Access the thread-local Cell directly.
/// Uses try_with to handle thread shutdown gracefully.
fn with_thread_cell<R>(mut f: impl FnMut(&Cell<Option<ContextStore>>) -> R) -> R {
    match CONTEXT.try_with(|cell| f(cell)) {
        Ok(r) => r,
        Err(_) => {
            // Thread-local is being destroyed — provide an empty Cell
            // so callers see None ("busy") and return defaults.
            let temp = Cell::new(None);
            f(&temp)
        }
    }
}

/// Execute `f` with exclusive access to the context store.
/// Returns `None` if the store is busy (re-entrant access).
///
/// The Cell is restored (`set(Some(store))`) before the return value is
/// dropped, so any user Drop code runs with a valid store in the Cell.
fn with_store<R>(f: impl FnOnce(&mut ContextStore) -> R) -> Option<R> {
    let f = std::cell::Cell::new(Some(f));
    with_thread_cell(|cell| {
        let mut store = cell.take()?; // None = busy
        let func = f.take().expect("with_store closure called more than once");
        let result = func(&mut store);
        cell.set(Some(store));
        Some(result)
    })
}

// ── Scope management ───────────────────────────────────────────

/// Push a new scope and return a guard.
/// Returns a no-op guard if the store is busy (re-entrant access).
pub fn enter_scope() -> ScopeGuard {
    with_store(|store| ScopeGuard::new(store.push_scope(None)))
        .unwrap_or_else(ScopeGuard::noop)
}

/// Push a new **named** scope and return a guard.
/// Returns a no-op guard if the store is busy (re-entrant access).
///
/// Named scopes appear in [`scope_chain()`] — they form a lightweight call
/// stack that is propagated across process boundaries.
pub fn enter_named_scope(name: impl Into<String>) -> ScopeGuard {
    let name = name.into();
    with_store(|store| ScopeGuard::new(store.push_scope(Some(name))))
        .unwrap_or_else(ScopeGuard::noop)
}

/// Pop a scope (called by ScopeGuard::drop).
/// Silently skips if the store is busy (re-entrant access) or if the
/// guard is a noop sentinel (expected_depth == usize::MAX).
pub(crate) fn leave_scope(expected_depth: usize) {
    if expected_depth == usize::MAX {
        return; // noop guard
    }
    // Garbage (old Arc values) is returned from with_store and dropped here,
    // outside the Cell window, so any user Drop code runs with a valid store.
    let _garbage = with_store(|store| store.pop_scope(expected_depth));
}

/// Execute `f` in a new scope. Changes revert when `f` returns.
pub fn scope<R>(f: impl FnOnce() -> R) -> R {
    let _guard = enter_scope();
    f()
}

// ── Deprecated compatibility ───────────────────────────────────

/// No-op compatibility shim. With the dual-context redesign, there is no
/// dispatch logic to override — all access in this module is already
/// thread-local. This function simply calls `f()` and returns the result.
#[deprecated(note = "No longer needed: sync_storage always uses thread-local. Will be removed in a future release.")]
pub fn force_thread_local<R>(f: impl FnOnce() -> R) -> R {
    f()
}

/// Deprecated: use `async_ctx::scope()` instead.
/// For backward compatibility, dispatches to task-local if available.
#[deprecated(note = "Use async_ctx::scope() for task-local scoping.")]
pub async fn scope_async<F, R>(f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    // If task-local is available, use it (old dispatch behavior)
    if crate::async_ctx::current_depth().is_some() {
        return crate::async_ctx::scope("", f).await;
    }
    let depth = with_store(|store| store.push_scope(None));

    match depth {
        None => f.await,
        Some(depth) => {
            let cleanup = ScopeCleanup(depth);
            let result = f.await;
            std::mem::forget(cleanup);
            leave_scope(depth);
            result
        }
    }
}

/// Deprecated: use `async_ctx::scope()` instead.
/// For backward compatibility, dispatches to task-local if available.
#[deprecated(note = "Use async_ctx::scope() for task-local scoping.")]
pub async fn named_scope_async<F, R>(name: impl Into<String>, f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    let name = name.into();
    // If task-local is available, use it (old dispatch behavior)
    if crate::async_ctx::current_depth().is_some() {
        return crate::async_ctx::scope(&name, f).await;
    }
    let depth = with_store(|store| store.push_scope(Some(name)));

    match depth {
        None => f.await,
        Some(depth) => {
            let cleanup = ScopeCleanup(depth);
            let result = f.await;
            std::mem::forget(cleanup);
            leave_scope(depth);
            result
        }
    }
}

// ── Fork support ───────────────────────────────────────────────

/// Create a ForkHandle from the current context state.
/// Returns None if the store is busy.
pub(crate) fn do_fork() -> Option<crate::fork::ForkHandle> {
    with_store(|store| crate::fork::create_fork_handle(store))
}

/// RAII guard that pops a scope during unwind if the async future panics.
/// On the normal path the caller calls `std::mem::forget(cleanup)` to disarm it.
struct ScopeCleanup(usize);

impl Drop for ScopeCleanup {
    fn drop(&mut self) {
        leave_scope(self.0);
    }
}

// ── Value access ───────────────────────────────────────────────

/// Get a value from the context. Returns an Arc clone.
/// Returns `None` if the key is not set or the store is busy.
pub(crate) fn get_value(key: &str) -> Option<Arc<dyn ContextValue>> {
    with_store(|store| store.get_value(key)).flatten()
}

/// Set a value in the current scope.
/// Silently skips if the store is busy (re-entrant access).
/// Old value is dropped outside the Cell window.
pub(crate) fn set_value(key: &'static str, value: Arc<dyn ContextValue>) {
    let _old = with_store(|store| store.set_value(key, value));
    // _old: Option<Option<Arc<dyn ContextValue>>> — dropped here, outside Cell window
}

/// Collect all effective values (for snapshot/serialization).
/// Returns an empty map if the store is busy.
pub(crate) fn collect_values() -> HashMap<&'static str, Arc<dyn ContextValue>> {
    with_store(|store| store.collect_values())
        .unwrap_or_default()
}

/// Return the current scope chain: remote prefix + local named scope names.
///
/// Returns an empty `Vec` if the store is busy (re-entrant access from
/// tracing callbacks, etc.).
pub fn scope_chain() -> Vec<String> {
    with_store(|store| store.scope_chain())
        .unwrap_or_default()
}

/// Collect the scope chain for serialization.
pub(crate) fn collect_scope_chain() -> Vec<String> {
    scope_chain()
}

/// Store a remote scope chain in the current context.
/// Silently skips if the store is busy (re-entrant access).
pub(crate) fn set_remote_chain(chain: Vec<String>) {
    with_store(|store| store.set_remote_chain(chain));
}


