//! Attach guards for swapping entire context stores.
//!
//! Unlike `ScopeGuard` (which pushes/pops a scope within the current store),
//! `AttachGuard` replaces the entire thread-local store and restores it on drop.

use crate::store::ContextStore;
use crate::store::CONTEXT;

/// RAII guard that restores the previous thread-local store on drop.
///
/// Created by [`attach_snapshot`](crate::attach_snapshot) or
/// [`attach_store`](crate::attach_store).
///
/// # `!Send` constraint
///
/// This guard is `!Send` — it **must** be dropped on the same thread where it was created.
/// Holding it across `.await` in a multi-threaded runtime will cause a compile error.
///
/// For async code, use `.attach(snap)` or `.scope("name")` from [`ContextFutureExt`](crate::ContextFutureExt)
/// instead of manually creating guards.
pub struct AttachGuard {
    prev: Option<ContextStore>,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl AttachGuard {
    pub(crate) fn new(prev: Option<ContextStore>) -> Self {
        Self {
            prev,
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        std::thread::LocalKey::with(&CONTEXT, |cell| {
            cell.set(self.prev.take());
        });
    }
}
