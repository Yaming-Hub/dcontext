//! Async-aware tracing layer that writes to the task-local context store.
//!
//! `AsyncDcontextLayer` pushes/pops scopes on the **task-local** store.
//! Unlike sync tracing, async spans enter/exit on every poll boundary
//! (due to `Instrumented<F>`). This layer handles that correctly by:
//!
//! - Pushing a scope on **first enter** of a span (per task)
//! - NOT popping on exit (scope persists across yields)
//! - Popping on **close** (span fully completes)
//!
//! This ensures scopes follow the logical async lifetime, not individual polls.

use std::cell::RefCell;
use std::collections::HashSet;
use std::marker::PhantomData;

use tracing::Subscriber;
use tracing_core::span;
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

// Track which span IDs have active scopes in the current task-local context.
// This prevents double-pushing on re-enter after yield.
thread_local! {
    static ACTIVE_ASYNC_SPANS: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
    static ASYNC_SCOPE_DEPTHS: RefCell<Vec<(u64, usize)>> = RefCell::new(Vec::new());
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
        let span_id = id.into_u64();

        // Only push a scope on the FIRST enter (not re-enters after yield)
        let already_active = ACTIVE_ASYNC_SPANS.with(|set| {
            let mut set = set.borrow_mut();
            !set.insert(span_id) // returns false if newly inserted (not previously active)
        });

        if already_active {
            return; // Re-enter after yield — scope already exists
        }

        // Push scope on task-local store
        let name = ctx
            .span(id)
            .map(|s| s.metadata().name())
            .unwrap_or("unknown");

        let guard = dcontext::async_ctx::push_scope(name);

        // Store the depth for manual pop on close
        let depth = guard.expected_depth();
        std::mem::forget(guard); // We'll manually pop on close

        ASYNC_SCOPE_DEPTHS.with(|depths| {
            depths.borrow_mut().push((span_id, depth));
        });
    }

    fn on_exit(&self, _id: &span::Id, _ctx: Context<'_, S>) {
        // Intentionally do nothing — scopes persist across yields for async spans
    }

    fn on_close(&self, id: span::Id, _ctx: Context<'_, S>) {
        let span_id = id.into_u64();

        // Remove from active set
        ACTIVE_ASYNC_SPANS.with(|set| {
            set.borrow_mut().remove(&span_id);
        });

        // Pop the scope by depth
        let depth = ASYNC_SCOPE_DEPTHS.with(|depths| {
            let mut depths = depths.borrow_mut();
            depths
                .iter()
                .rposition(|(sid, _)| *sid == span_id)
                .map(|pos| depths.remove(pos).1)
        });

        if let Some(depth) = depth {
            dcontext::async_ctx::pop_scope(depth);
        }
    }
}

