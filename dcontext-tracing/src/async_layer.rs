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
//!
//! This layer supports four levels of integration:
//!
//! 1. **Auto-scoping**: Every span creates a task-local dcontext scope.
//!
//! 2. **Field extraction**: Extract tracing span fields into task-local context.
//!    Configure via [`TracingField`](crate::TracingField) metadata.
//!
//! 3. **Span info**: Optionally expose span metadata as a
//!    [`SpanInfo`](crate::SpanInfo) context value.
//!
//! 4. **Span recording**: Auto-record context values into pre-declared Empty
//!    span fields.

use std::marker::PhantomData;

use tracing::Subscriber;
use tracing_core::span;
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use crate::field_mapping::ExtractedFields;
use crate::layer_common;

/// Per-span state stored in span extensions.
/// Tracks whether this span has pushed a scope and at what depth.
struct AsyncScopeState {
    /// The depth returned by push_scope — serves as a unique scope ID
    /// for verifying the correct scope is being popped on close.
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
    include_span_info: bool,
    _subscriber: PhantomData<fn(S)>,
}

impl<S> AsyncDcontextLayer<S> {
    /// Create a new `AsyncDcontextLayer` with default settings (auto-scoping only).
    ///
    /// Field extraction is auto-discovered from [`TracingField`](crate::TracingField)
    /// metadata in the registry. No explicit field configuration is needed.
    pub fn new() -> Self {
        Self {
            include_span_info: false,
            _subscriber: PhantomData,
        }
    }

    /// Create an [`AsyncDcontextLayerBuilder`] for configuring the layer.
    pub fn builder() -> AsyncDcontextLayerBuilder<S> {
        AsyncDcontextLayerBuilder {
            include_span_info: false,
            _subscriber: PhantomData,
        }
    }
}

impl<S> Default for AsyncDcontextLayer<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for configuring an [`AsyncDcontextLayer`].
pub struct AsyncDcontextLayerBuilder<S> {
    include_span_info: bool,
    _subscriber: PhantomData<fn(S)>,
}

impl<S> AsyncDcontextLayerBuilder<S> {
    /// Include span metadata (name, target, level) as a [`SpanInfo`](crate::SpanInfo) context value.
    ///
    /// When enabled, each span's first enter will set a [`SpanInfo`] value under
    /// the key `"dcontext.span"` in the task-local store.
    pub fn include_span_info(mut self) -> Self {
        self.include_span_info = true;
        self
    }

    /// Build the configured [`AsyncDcontextLayer`].
    pub fn build(self) -> AsyncDcontextLayer<S> {
        AsyncDcontextLayer {
            include_span_info: self.include_span_info,
            _subscriber: PhantomData,
        }
    }
}

impl<S> Layer<S> for AsyncDcontextLayer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &span::Attributes<'_>,
        id: &span::Id,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            layer_common::extract_span_fields(attrs, &span);
        }
    }

    fn on_record(
        &self,
        id: &span::Id,
        values: &span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            layer_common::merge_recorded_fields(values, &span);
        }
    }

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

        // Level 2: Apply metadata-driven extraction into task-local context
        layer_common::apply_field_extraction(&span);

        // Level 3: Set span info in task-local context
        if self.include_span_info {
            layer_common::set_span_info(&span);
        }

        // Level 4: Record context values into span fields
        layer_common::record_context_to_span(&span);

        // Store depth (scope ID) in span extensions for verification on close
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
            // Only pop if the current scope depth matches what we pushed.
            // Depth is a unique scope ID — this guards against mismatched pops
            // if scopes were manipulated externally between enter and close.
            let current = dcontext::async_ctx::current_depth();
            if current == Some(state.depth) {
                dcontext::async_ctx::pop_scope(state.depth);
            }
        }

        // Clean up extracted fields
        {
            let mut extensions = span.extensions_mut();
            extensions.remove::<ExtractedFields>();
        }
    }
}

