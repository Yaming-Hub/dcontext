//! Async-aware tracing layer that writes to the task-local context store.
//!
//! `AsyncDcontextLayer` pushes/pops scopes on the **task-local** store.
//! Unlike sync tracing, async spans enter/exit on every poll boundary
//! (due to `Instrumented<F>`). This layer handles that correctly by:
//!
//! - Pushing a scope on **first enter** of a span (tracked via span extensions)
//! - NOT popping on exit (scope persists across yields)
//! - Popping on **close** (span fully completes)
//!
//! This ensures scopes follow the logical async lifetime, not individual polls.
//! State is stored in span extensions (not thread-local), so it correctly
//! handles task migration across threads in multi-threaded runtimes.

use std::marker::PhantomData;

use tracing::Subscriber;
use tracing_core::span;
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

/// Per-span state stored in span extensions.
/// Tracks whether this span has pushed a scope and at what depth.
struct AsyncScopeState {
    /// The depth returned by push_scope — needed for pop_scope on close.
    depth: usize,
}

/// A tracing layer that manages dcontext scopes on the **task-local** store.
///
/// Designed for async code: scopes persist across yields and are only
/// cleaned up when the span closes.
///
/// On first span enter: pushes a named scope via `async_ctx::push_scope(span_name)`
/// On span close: pops the scope
///
/// If not in an async task (`TASK_CONTEXT.try_with` fails), silently no-ops.
///
/// # Example
///
/// ```rust,ignore
/// use tracing_subscriber::prelude::*;
///
/// tracing_subscriber::registry()
///     .with(dcontext_tracing::AsyncDcontextLayer::new())
///     .init();
/// ```
pub struct AsyncDcontextLayer<S> {
    _subscriber: PhantomData<fn(S)>,
}

impl<S> AsyncDcontextLayer<S> {
    /// Create a new `AsyncDcontextLayer`.
    pub fn new() -> Self {
        Self {
            _subscriber: PhantomData,
        }
    }
}

impl<S> Default for AsyncDcontextLayer<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for AsyncDcontextLayer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        let span = match ctx.span(id) {
            Some(s) => s,
            None => return,
        };

        // Check if we've already pushed a scope for this span (re-enter after yield).
        // State is stored in span extensions — safe across thread migration.
        {
            let extensions = span.extensions();
            if extensions.get::<AsyncScopeState>().is_some() {
                return; // Already active — skip re-push
            }
        }

        // First enter: push scope on task-local store.
        // If not in an async task (no task-local context), try_push_scope returns
        // None and the layer is completely transparent outside tokio.
        let name = span.metadata().name();
        let guard = match dcontext::async_ctx::try_push_scope(name) {
            Some(guard) => guard,
            None => return, // No task-local context — do nothing
        };
        let depth = guard.expected_depth();
        std::mem::forget(guard); // We'll manually pop on close

        // Store depth in span extensions for retrieval on close
        let mut extensions = span.extensions_mut();
        extensions.insert(AsyncScopeState { depth });
    }

    fn on_exit(&self, _id: &span::Id, _ctx: Context<'_, S>) {
        // Intentionally do nothing — scopes persist across yields for async spans
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        let span = match ctx.span(&id) {
            Some(s) => s,
            None => return,
        };

        // Retrieve and remove the scope state
        let state = {
            let mut extensions = span.extensions_mut();
            extensions.remove::<AsyncScopeState>()
        };

        if let Some(state) = state {
            dcontext::async_ctx::pop_scope(state.depth);
        }
    }
}

