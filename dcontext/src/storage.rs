use std::cell::RefCell;

use crate::scope::{ContextStack, ScopeGuard};
use crate::value::ContextValue;

thread_local! {
    pub(crate) static CONTEXT: RefCell<ContextStack> = RefCell::new(ContextStack::new());
    static FORCE_THREAD_LOCAL_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "tokio")]
tokio::task_local! {
    pub(crate) static TASK_CONTEXT: RefCell<ContextStack>;
}

/// Access the current context stack, dispatching between task-local and
/// thread-local storage. Uses FnMut to allow the closure to be called
/// in either branch without move issues.
pub(crate) fn with_current_stack<R>(mut f: impl FnMut(&RefCell<ContextStack>) -> R) -> R {
    // Try task-local first (async path).
    #[cfg(feature = "tokio")]
    {
        let force = FORCE_THREAD_LOCAL_DEPTH.with(|c| c.get()) > 0;
        if !force {
            // Use a Cell to smuggle the result out of the closure.
            let result: std::cell::Cell<Option<R>> = std::cell::Cell::new(None);
            let found = TASK_CONTEXT.try_with(|stack| {
                result.set(Some(f(stack)));
            });
            if found.is_ok() {
                return result.into_inner()
                    .expect("invariant: closure set the result when try_with succeeded");
            }

            // No task-local found inside Tokio runtime.
            // Fall through to thread-local storage instead of panicking.
            // This is the correct behavior for startup code, tracing layer
            // callbacks, and fire-and-forget tasks that have no request context.
        }
    }

    // Sync path or async-without-task-local — use thread-local.
    CONTEXT.with(|stack| f(stack))
}

/// Push a new scope and return a guard.
/// Returns a no-op guard if the RefCell is already borrowed (re-entrant access).
pub fn enter_scope() -> ScopeGuard {
    with_current_stack(|cell| match cell.try_borrow_mut() {
        Ok(mut stack) => {
            let (id, depth) = stack.push_scope();
            ScopeGuard::new(id, depth)
        }
        Err(_) => ScopeGuard::noop(),
    })
}

/// Push a new **named** scope and return a guard.
/// Returns a no-op guard if the RefCell is already borrowed (re-entrant access).
///
/// Named scopes appear in [`scope_chain()`] — they form a lightweight call
/// stack that is propagated across process boundaries.
pub fn enter_named_scope(name: impl Into<String>) -> ScopeGuard {
    let name = name.into();
    with_current_stack(|cell| match cell.try_borrow_mut() {
        Ok(mut stack) => {
            let (id, depth) = stack.push_named_scope(name.clone());
            ScopeGuard::new(id, depth)
        }
        Err(_) => ScopeGuard::noop(),
    })
}

/// Pop a scope (called by ScopeGuard::drop).
/// Silently skips if the RefCell is already borrowed (re-entrant access)
/// or if the guard is a noop sentinel (expected_depth == usize::MAX).
pub(crate) fn leave_scope(expected_depth: usize) {
    if expected_depth == usize::MAX {
        return; // noop guard
    }
    with_current_stack(|cell| {
        if let Ok(mut stack) = cell.try_borrow_mut() {
            stack.pop_scope(expected_depth);
        }
    })
}

/// Execute `f` in a new scope. Changes revert when `f` returns.
pub fn scope<R>(f: impl FnOnce() -> R) -> R {
    let _guard = enter_scope();
    f()
}

/// RAII guard that pops a scope during unwind if the async future panics.
/// On the normal path, the caller calls `std::mem::forget(cleanup)` to disarm it.
#[cfg(feature = "tokio")]
struct ScopeCleanup(usize);

#[cfg(feature = "tokio")]
impl Drop for ScopeCleanup {
    fn drop(&mut self) {
        with_current_stack(|cell| {
            if let Ok(mut stack) = cell.try_borrow_mut() {
                stack.pop_scope(self.0);
            }
            // If borrow fails during unwind, scope leaks — acceptable.
        });
    }
}

/// Async version: execute a future in a new scope.
/// The scope is entered before polling and exited after the future completes.
/// If the future panics, the scope is cleaned up during unwind.
#[cfg(feature = "tokio")]
pub async fn scope_async<F, R>(f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    let depth = with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        let (_id, depth) = stack.push_scope();
        depth
    });

    let cleanup = ScopeCleanup(depth);
    let result = f.await;

    // Normal path: disarm the cleanup guard and pop scope explicitly.
    std::mem::forget(cleanup);
    with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        stack.pop_scope(depth);
    });

    result
}

/// Async version of [`enter_named_scope`]: execute a future in a new **named** scope.
///
/// Like [`scope_async`], this avoids holding the `!Send` [`ScopeGuard`] across
/// `.await` points by manually managing push/pop. If the future panics, the
/// scope is cleaned up during unwind.
///
/// Named scopes appear in [`scope_chain()`] and propagate across process
/// boundaries via serialization.
#[cfg(feature = "tokio")]
pub async fn named_scope_async<F, R>(name: impl Into<String>, f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    let name = name.into();
    let depth = with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        let (_id, depth) = stack.push_named_scope(name.clone());
        depth
    });

    let cleanup = ScopeCleanup(depth);
    let result = f.await;

    std::mem::forget(cleanup);
    with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        stack.pop_scope(depth);
    });

    result
}

/// Get a value from the context. Returns a clone.
/// Returns `None` if the RefCell is already mutably borrowed (re-entrant access).
pub(crate) fn get_value(key: &str) -> Option<Box<dyn ContextValue>> {
    with_current_stack(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|stack| stack.lookup(key).map(|v| v.clone_boxed()))
    })
}

/// Set a value in the current topmost scope.
/// Silently skips if the RefCell is already borrowed (re-entrant access).
/// Old value is dropped outside the RefCell borrow to prevent re-entrancy panics.
pub(crate) fn set_value(key: &'static str, value: Box<dyn ContextValue>) {
    let mut value = Some(value);
    let _old = with_current_stack(|cell| {
        match cell.try_borrow_mut() {
            Ok(mut stack) => {
                stack.set(key, value.take().expect("invariant: value is always Some on entry"))
            }
            Err(_) => None,
        }
    });
    // _old dropped here, outside the borrow — safe if Drop calls get_context
}

/// Collect all effective values (for snapshot/serialization).
/// Returns an empty map if the RefCell is already mutably borrowed.
pub(crate) fn collect_values() -> std::collections::HashMap<&'static str, Box<dyn ContextValue>> {
    with_current_stack(|cell| {
        match cell.try_borrow() {
            Ok(stack) => stack
                .merged_values()
                .into_iter()
                .map(|(k, v)| (k, v.clone_boxed()))
                .collect(),
            Err(_) => std::collections::HashMap::new(),
        }
    })
}

/// Return the current scope chain: remote prefix + local named scope names.
///
/// Returns an empty `Vec` if the context stack is currently borrowed by a
/// write operation (re-entrant access from tracing callbacks, etc.).
pub fn scope_chain() -> Vec<String> {
    with_current_stack(|cell| {
        match cell.try_borrow() {
            Ok(stack) => stack.scope_chain(),
            Err(_) => Vec::new(),
        }
    })
}

/// Collect the scope chain for serialization.
pub(crate) fn collect_scope_chain() -> Vec<String> {
    scope_chain()
}

/// Store a remote scope chain in the current context stack.
/// Silently skips if the RefCell is already borrowed (re-entrant access).
pub(crate) fn set_remote_chain(chain: Vec<String>) {
    with_current_stack(|cell| {
        if let Ok(mut stack) = cell.try_borrow_mut() {
            stack.set_remote_chain(chain.clone());
        }
    })
}

/// Escape hatch: explicitly use thread-local storage even inside an async runtime.
/// Panic-safe and nesting-safe via depth counter + RAII guard.
pub fn force_thread_local<R>(f: impl FnOnce() -> R) -> R {
    FORCE_THREAD_LOCAL_DEPTH.with(|c| c.set(c.get() + 1));

    struct DepthGuard;
    impl Drop for DepthGuard {
        fn drop(&mut self) {
            crate::storage::FORCE_THREAD_LOCAL_DEPTH.with(|c| c.set(c.get() - 1));
        }
    }
    let _guard = DepthGuard;

    f()
}
