//! Future extension for async context propagation.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;

use crate::store::{ContextStore, CONTEXT};

pin_project! {
    /// A future wrapped with a `ContextStore` that is swapped into the
    /// thread-local on each poll and swapped back out after.
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

    /// Attach a `ContextSnapshot` as the active store for this future.
    ///
    /// Equivalent to `.with_context(snap.into())`.
    /// Use at inbound boundaries where a deserialized snapshot needs to
    /// become the active context.
    fn attach(self, snap: crate::ContextSnapshot) -> WithContext<Self> {
        self.with_context(snap.into())
    }

    /// Fork the current context and wrap this future with the forked child.
    ///
    /// The child inherits parent values via frozen parent (cheap, Arc-shared).
    /// Writes in the child are isolated from the parent.
    fn fork(self) -> WithContext<Self> {
        self.with_context(crate::fork())
    }

    /// Capture the current context and wrap this future with it.
    ///
    /// Creates a snapshot (excluding local-only variables) and converts
    /// it to a store.
    fn capture(self) -> WithContext<Self> {
        let snap = crate::capture();
        self.with_context(snap.into())
    }

    /// Push a named scope into the given store and wrap this future.
    ///
    /// Compose with other wrappers:
    /// ```ignore
    /// next.scope("remote:MyActor").attach(snap).await
    /// ```
    fn scope(self, name: &str) -> WithContext<Self> {
        let mut store = crate::fork();
        store.push_scope(Some(name.to_string()));
        self.with_context(store)
    }
}

impl<F: Sized> ContextFutureExt for F {}
