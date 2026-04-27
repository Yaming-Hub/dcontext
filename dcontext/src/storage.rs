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

            // No task-local. Are we inside an async runtime?
            // (Skip this check during tests to avoid false panics)
            #[cfg(not(test))]
            if tokio::runtime::Handle::try_current().is_ok() {
                panic!(
                    "dcontext: context accessed inside Tokio runtime without \
                     with_context(). Wrap your task with \
                     dcontext::spawn_with_context_async() or dcontext::with_context()."
                );
            }
        }
    }

    // Pure sync path — use thread-local.
    CONTEXT.with(|stack| f(stack))
}

/// Push a new scope and return a guard.
pub fn enter_scope() -> ScopeGuard {
    with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        let (id, depth) = stack.push_scope();
        ScopeGuard::new(id, depth)
    })
}

/// Push a new **named** scope and return a guard.
///
/// Named scopes appear in [`scope_chain()`] — they form a lightweight call
/// stack that is propagated across process boundaries.
pub fn enter_named_scope(name: impl Into<String>) -> ScopeGuard {
    let name = name.into();
    with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        let (id, depth) = stack.push_named_scope(name.clone());
        ScopeGuard::new(id, depth)
    })
}

/// Pop a scope (called by ScopeGuard::drop).
pub(crate) fn leave_scope(expected_depth: usize) {
    with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        stack.pop_scope(expected_depth);
    })
}

/// Execute `f` in a new scope. Changes revert when `f` returns.
pub fn scope<R>(f: impl FnOnce() -> R) -> R {
    let _guard = enter_scope();
    f()
}

/// Async version: execute a future in a new scope.
/// The scope is entered before polling and exited after the future completes.
#[cfg(feature = "tokio")]
pub async fn scope_async<F, R>(f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    // Enter scope and record the depth, but don't hold the !Send guard
    // across the .await point. Instead, manually manage enter/leave.
    let depth = with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        let (_id, depth) = stack.push_scope();
        depth
    });

    let result = f.await;

    // Manually pop the scope after the future completes.
    with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        stack.pop_scope(depth);
    });

    result
}

/// Async version of [`enter_named_scope`]: execute a future in a new **named** scope.
///
/// Like [`scope_async`], this avoids holding the `!Send` [`ScopeGuard`] across
/// `.await` points by manually managing push/pop.
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

    let result = f.await;

    with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        stack.pop_scope(depth);
    });

    result
}

/// Get a value from the context. Returns a clone.
/// The RefCell borrow is released before cloning user data (C3 safety).
pub(crate) fn get_value(key: &str) -> Option<Box<dyn ContextValue>> {
    with_current_stack(|cell| {
        let stack = cell.borrow();
        stack.lookup(key).map(|v| v.clone_boxed())
        // borrow released here
    })
}

/// Set a value in the current topmost scope.
/// Old value is dropped outside the RefCell borrow to prevent re-entrancy panics (B3).
pub(crate) fn set_value(key: &'static str, value: Box<dyn ContextValue>) {
    let mut value = Some(value);
    let _old = with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        stack.set(key, value.take().expect("invariant: value is always Some on entry"))
        // old value returned here, borrow released
    });
    // _old dropped here, outside the borrow — safe if Drop calls get_context
}

/// Collect all effective values (for snapshot/serialization).
pub(crate) fn collect_values() -> std::collections::HashMap<&'static str, Box<dyn ContextValue>> {
    with_current_stack(|cell| {
        let stack = cell.borrow();
        stack
            .merged_values()
            .into_iter()
            .map(|(k, v)| (k, v.clone_boxed()))
            .collect()
    })
}

/// Return the current scope chain: remote prefix + local named scope names.
///
/// The scope chain is a lightweight representation of the execution path —
/// similar to a call stack but expressed as a list of named scopes. It
/// includes scope names from remote callers that were propagated via
/// serialization, followed by local named scopes in the current process.
///
/// Unnamed scopes (created with [`enter_scope()`]) are invisible in the chain.
pub fn scope_chain() -> Vec<String> {
    with_current_stack(|cell| {
        let stack = cell.borrow();
        stack.scope_chain()
    })
}

/// Collect the scope chain for serialization.
pub(crate) fn collect_scope_chain() -> Vec<String> {
    scope_chain()
}

/// Store a remote scope chain in the current context stack.
/// The previous chain is saved on the topmost scope for LIFO restoration.
pub(crate) fn set_remote_chain(chain: Vec<String>) {
    with_current_stack(|cell| {
        let mut stack = cell.borrow_mut();
        stack.set_remote_chain(chain.clone());
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
