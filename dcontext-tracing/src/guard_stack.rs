use std::cell::RefCell;

use tracing_core::span;

// Thread-local stack of dcontext scope guards, keyed by span ID.
// Stores (span::Id, ScopeGuard) pairs. Guards are pushed on `on_enter`
// and popped on `on_exit`. Using a thread-local avoids the !Send problem
// with ScopeGuard (span extensions require Send + Sync).
thread_local! {
    static SCOPE_GUARDS: RefCell<Vec<(u64, dcontext::ScopeGuard)>> = RefCell::new(Vec::new());
}

/// Push a new scope guard for the given span.
pub(crate) fn push_guard(id: &span::Id, guard: dcontext::ScopeGuard) {
    SCOPE_GUARDS.with(|stack| {
        stack.borrow_mut().push((id.into_u64(), guard));
    });
}

/// Pop the most recent scope guard matching the given span ID.
///
/// Searches from the back (most recent) to handle nesting correctly.
/// The guard is dropped, which reverts the dcontext scope.
pub(crate) fn pop_guard(id: &span::Id) {
    SCOPE_GUARDS.with(|stack| {
        let mut stack = stack.borrow_mut();
        let target = id.into_u64();
        // Search from back — most recent entry for this span
        if let Some(pos) = stack.iter().rposition(|(sid, _)| *sid == target) {
            stack.remove(pos);
            // guard dropped here → scope reverts
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_single() {
        let _guard = dcontext::enter_scope();
        let id = span::Id::from_u64(1);
        let scope_guard = dcontext::enter_scope();
        push_guard(&id, scope_guard);

        // Verify stack has one entry
        SCOPE_GUARDS.with(|s| assert_eq!(s.borrow().len(), 1));

        pop_guard(&id);
        SCOPE_GUARDS.with(|s| assert!(s.borrow().is_empty()));
    }

    #[test]
    fn push_pop_nested() {
        let _guard = dcontext::enter_scope();
        let id1 = span::Id::from_u64(1);
        let id2 = span::Id::from_u64(2);

        push_guard(&id1, dcontext::enter_scope());
        push_guard(&id2, dcontext::enter_scope());

        SCOPE_GUARDS.with(|s| assert_eq!(s.borrow().len(), 2));

        // Pop in reverse order (normal case)
        pop_guard(&id2);
        SCOPE_GUARDS.with(|s| assert_eq!(s.borrow().len(), 1));

        pop_guard(&id1);
        SCOPE_GUARDS.with(|s| assert!(s.borrow().is_empty()));
    }

    #[test]
    fn pop_nonexistent_is_noop() {
        let id = span::Id::from_u64(99);
        pop_guard(&id); // should not panic
    }
}
