//! Future extension for async context propagation.
//!
//! `WithContext<F>` wraps a future, swapping a `ContextStore` in/out of the
//! thread-local on each poll. This is runtime-agnostic - works on tokio,
//! async-std, smol, or any executor.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;

use crate::store::ContextStore;
use crate::sync_ctx::CONTEXT;

pin_project! {
    /// A future wrapped with a `ContextStore` that is swapped into the
    /// thread-local on each poll and swapped back out after.
    ///
    /// Mutations to the context within the future persist across `.await`
    /// points because the store is owned by this wrapper.
    pub struct WithContext<F> {
        #[pin]
        inner: F,
        store: Option<ContextStore>,
    }
}

impl<F: Future> Future for WithContext<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let prev = CONTEXT.with(|cell| cell.replace(this.store.take()));
        let result = this.inner.poll(cx);
        *this.store = CONTEXT.with(|cell| cell.replace(prev));
        result
    }
}

/// Extension trait providing context propagation for futures.
///
/// Available on all `Sized` types - typically used on futures before spawning.
pub trait ContextFutureExt: Sized {
    /// Wrap this future with a specific `ContextStore`.
    ///
    /// On each poll, the store is swapped into thread-local storage.
    /// Mutations within the future persist across await points.
    fn with_context(self, store: ContextStore) -> WithContext<Self> {
        WithContext {
            inner: self,
            store: Some(store),
        }
    }

    /// Fork the current context and wrap this future with the forked child.
    ///
    /// Equivalent to `self.with_context(dcontext::fork())`.
    /// The child inherits parent values via frozen parent (cheap, Arc-shared).
    /// Writes in the child are isolated from the parent.
    fn fork_context(self) -> WithContext<Self> {
        let store = crate::fork();
        self.with_context(store)
    }

    /// Snapshot the current context and wrap this future with it.
    ///
    /// Creates a full deep copy of all effective values. Use when the future
    /// needs complete independence from the parent or when values need to
    /// be serialization-independent.
    fn capture_context(self) -> WithContext<Self> {
        let snap = crate::snapshot();
        let store = crate::snapshot_to_store(snap);
        self.with_context(store)
    }

    /// Fork the current context, push a named scope, and wrap this future.
    ///
    /// Like `fork_context()` but also creates a named scope that appears in
    /// `scope_chain()`. Ideal for request/task boundaries where you want
    /// the scope name to be visible for debugging and tracing.
    fn fork_scope(self, name: &str) -> WithContext<Self> {
        let mut store = crate::fork();
        store.push_scope(Some(name.to_string()));
        self.with_context(store)
    }

    /// Snapshot the current context, push a named scope, and wrap this future.
    ///
    /// Like `capture_context()` but also creates a named scope that appears in
    /// `scope_chain()`. Use at boundaries where you need a full deep copy
    /// plus a visible scope name.
    fn capture_scope(self, name: &str) -> WithContext<Self> {
        let snap = crate::snapshot();
        let mut store = crate::snapshot_to_store(snap);
        store.push_scope(Some(name.to_string()));
        self.with_context(store)
    }
}

impl<F: Sized> ContextFutureExt for F {}
