use std::collections::HashMap;
use std::sync::Arc;

use crate::value::ContextValue;

// Immutable scope node (frozen parent scopes)

/// A frozen scope in the parent chain. Immutable after creation.
/// `Send + Sync` - safe inside `Arc`.
pub(crate) struct ScopeNode {
    pub(crate) name: Option<String>,
    pub(crate) values: HashMap<&'static str, Arc<dyn ContextValue>>,
    pub(crate) parent: Option<Arc<ScopeNode>>,
    pub(crate) depth: usize,
    pub(crate) remote_chain: Arc<Vec<String>>,
    pub(crate) remote_chain_base_depth: usize,
    pub(crate) saved_scope_barrier: Option<usize>,
}

// Garbage bag

pub(crate) struct ScopeGarbage {
    pub(crate) _old_values: HashMap<&'static str, Arc<dyn ContextValue>>,
}

// Scope guard

/// RAII guard that reverts a scope on drop.
///
/// # `!Send` constraint
///
/// This guard is `!Send` — it **must** be dropped on the same thread where it was created.
/// Holding it across `.await` in a multi-threaded runtime will cause a compile error.
///
/// For async code, use `.scope("name")` from [`ContextFutureExt`](crate::ContextFutureExt)
/// instead of calling [`push_scope`](crate::push_scope) directly.
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

    pub(crate) fn noop() -> Self {
        Self {
            expected_depth: usize::MAX,
            _not_send: std::marker::PhantomData,
        }
    }

    pub fn expected_depth(&self) -> usize {
        self.expected_depth
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        leave_scope(self.expected_depth);
    }
}

fn leave_scope(expected_depth: usize) {
    if expected_depth == usize::MAX {
        return;
    }
    let _garbage = crate::store::try_apply(|store| store.pop_scope(expected_depth));
}
