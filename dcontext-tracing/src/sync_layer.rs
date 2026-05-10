//! Sync tracing layer that writes to the thread-local context store.
//!
//! `SyncDcontextLayer` pushes/pops scopes on the **thread-local** store.
//! It always succeeds (thread-local is always available).
//!
//! Use this layer for synchronous code or when you need span-scoped
//! context on a blocking thread.

use std::marker::PhantomData;

use tracing::Subscriber;
use tracing_core::span;
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use crate::guard_stack;

/// A tracing layer that manages dcontext scopes on the **thread-local** store.
///
/// On span enter: pushes a named scope via `sync_ctx::push_scope(span_name)`
/// On span exit: pops the scope via guard drop
///
/// Always succeeds — thread-local storage is always available.
///
/// # Example
///
/// ```rust,ignore
/// use tracing_subscriber::prelude::*;
///
/// tracing_subscriber::registry()
///     .with(dcontext_tracing::SyncDcontextLayer::new())
///     .init();
/// ```
pub struct SyncDcontextLayer<S> {
    _subscriber: PhantomData<fn(S)>,
}

impl<S> SyncDcontextLayer<S> {
    /// Create a new `SyncDcontextLayer`.
    pub fn new() -> Self {
        Self {
            _subscriber: PhantomData,
        }
    }
}

impl<S> Default for SyncDcontextLayer<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for SyncDcontextLayer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        // Push a named scope on the thread-local store.
        let guard = if let Some(span) = ctx.span(id) {
            let name = span.metadata().name();
            dcontext::sync_ctx::push_scope(name)
        } else {
            dcontext::sync_ctx::push_scope("unknown")
        };

        guard_stack::push_guard(id, guard);
    }

    fn on_exit(&self, id: &span::Id, _ctx: Context<'_, S>) {
        guard_stack::pop_guard(id);
    }

    fn on_close(&self, id: span::Id, _ctx: Context<'_, S>) {
        guard_stack::pop_guard(&id);
    }
}
