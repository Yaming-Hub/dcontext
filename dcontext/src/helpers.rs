use crate::snapshot::{self, ContextSnapshot};

/// Spawn a std::thread that inherits the current context.
pub fn spawn_with_context<F, T>(name: &str, f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let snap = snapshot::snapshot();
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let _guard = snapshot::attach(snap);
            f()
        })
        .expect("failed to spawn thread")
}

/// Run an async block with the given snapshot established as task-local context.
#[cfg(feature = "tokio")]
pub async fn with_context<F, T>(snap: ContextSnapshot, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    use crate::scope::ContextStack;
    use crate::storage::TASK_CONTEXT;
    use std::cell::RefCell;

    let values = snap
        .values
        .iter()
        .map(|(k, v)| (*k, v.clone_boxed()))
        .collect();
    let stack = ContextStack::from_values(values);
    TASK_CONTEXT.scope(RefCell::new(stack), f).await
}

/// Spawn a Tokio task that inherits the current context.
#[cfg(feature = "tokio")]
pub fn spawn_with_context_async<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let snap = snapshot::snapshot();
    tokio::spawn(with_context(snap, future))
}
