use crate::snapshot::{self, ContextSnapshot};

/// Spawn a std::thread that inherits the current context.
/// Returns `io::Result` instead of panicking on spawn failure.
pub fn spawn_with_context<F, T>(name: &str, f: F) -> std::io::Result<std::thread::JoinHandle<T>>
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
}

/// Run an async block with the given snapshot established as task-local context.
pub async fn with_context<F, T>(snap: ContextSnapshot, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    use std::cell::Cell;
    use std::sync::Arc;

    use crate::sync_storage::ContextStore;
    use crate::async_storage::TASK_CONTEXT;

    let chain = snap.scope_chain.clone();
    let values = snap
        .values
        .iter()
        .map(|(k, v)| (*k, Arc::clone(v)))
        .collect();
    let store = ContextStore::from_values_with_chain(values, chain);
    TASK_CONTEXT.scope(Cell::new(Some(store)), f).await
}

/// Spawn a Tokio task that inherits the current context.
pub fn spawn_with_context_async<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let snap = snapshot::snapshot();
    tokio::spawn(with_context(snap, future))
}
