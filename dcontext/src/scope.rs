use std::collections::HashMap;
use std::sync::Arc;

use crate::value::ContextValue;

// ── Immutable scope node (frozen parent scopes) ────────────────

/// A frozen scope in the parent chain. Immutable after creation.
/// `Send + Sync` — safe inside `Arc`.
pub(crate) struct ScopeNode {
    /// Human-readable scope name. Named scopes appear in scope_chain().
    pub(crate) name: Option<String>,
    /// Values set in this scope (sparse — only keys modified here).
    pub(crate) values: HashMap<&'static str, Arc<dyn ContextValue>>,
    /// Link to parent scope.
    pub(crate) parent: Option<Arc<ScopeNode>>,
    /// Scope depth at the time this node was created.
    pub(crate) depth: usize,
    /// Remote chain state saved at scope entry.
    pub(crate) remote_chain: Arc<Vec<String>>,
    pub(crate) remote_chain_base_depth: usize,
}

// ── Garbage bag ────────────────────────────────────────────────

/// Holds old values that should be dropped outside the Cell window.
/// When this struct is dropped, Arc refcounts are decremented, potentially
/// running user Drop code. By that time, the Cell has been restored.
pub(crate) struct ScopeGarbage {
    pub(crate) _old_values: HashMap<&'static str, Arc<dyn ContextValue>>,
}

// ── Scope guard ────────────────────────────────────────────────

/// RAII guard that reverts a scope on drop.
/// Not Send/Sync — scopes are bound to their storage context.
pub struct ScopeGuard {
    expected_depth: usize,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ScopeGuard {
    pub(crate) fn new(expected_depth: usize) -> Self {
        Self {
            expected_depth,
            _not_send: std::marker::PhantomData,
        }
    }

    /// Create a no-op guard that does nothing on drop.
    /// Used when a scope cannot be pushed (re-entrant access / busy store).
    pub(crate) fn noop() -> Self {
        Self {
            expected_depth: usize::MAX,
            _not_send: std::marker::PhantomData,
        }
    }

    /// Return the expected depth for this guard.
    /// Used by layers that need to manually manage scope lifetimes.
    pub fn expected_depth(&self) -> usize {
        self.expected_depth
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        crate::storage::leave_scope(self.expected_depth);
    }
}
