//! Sync tracing layer that writes to the thread-local context store.
//!
//! `SyncDcontextLayer` pushes/pops scopes on the **thread-local** store.
//! It always succeeds (thread-local is always available).
//!
//! This layer supports four levels of integration:
//!
//! 1. **Auto-scoping** (zero config): Every span enter creates a new dcontext
//!    scope that inherits the parent scope's values. Reverted on span exit.
//!
//! 2. **Field extraction**: Extract tracing span fields into dcontext values.
//!    Configure via [`TracingField`](crate::TracingField) metadata at
//!    registration time.
//!
//! 3. **Span info**: Optionally expose span metadata (name, target, level) as
//!    a [`SpanInfo`](crate::SpanInfo) context value.
//!
//! 4. **Span recording**: Auto-record context values into pre-declared Empty
//!    span fields.

use std::marker::PhantomData;

use tracing::Subscriber;
use tracing_core::span;
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use crate::field_mapping::ExtractedFields;
use crate::guard_stack;
use crate::layer_common;

/// Marker stored in span extensions to record whether async context was
/// available when the span was first entered. This ensures consistent
/// skip behavior across enter/exit/close even if the execution context changes.
struct SyncSkipMarker {
    /// If true, async context was available on first enter — sync layer should skip.
    skip: bool,
}

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
    include_span_info: bool,
    /// When true, skip all work if an async task-local context is available.
    /// This allows mounting both AsyncDcontextLayer and SyncDcontextLayer
    /// without duplicating work — the async layer handles async tasks,
    /// and the sync layer only activates for pure sync code paths.
    async_aware: bool,
    _subscriber: PhantomData<fn(S)>,
}

impl<S> SyncDcontextLayer<S> {
    /// Create a new `SyncDcontextLayer` with default settings (auto-scoping only).
    ///
    /// Field extraction is auto-discovered from [`TracingField`](crate::TracingField)
    /// metadata in the registry. No explicit field configuration is needed.
    pub fn new() -> Self {
        Self {
            include_span_info: false,
            async_aware: false,
            _subscriber: PhantomData,
        }
    }

    /// Create a [`SyncDcontextLayerBuilder`] for configuring the layer.
    pub fn builder() -> SyncDcontextLayerBuilder<S> {
        SyncDcontextLayerBuilder {
            include_span_info: false,
            async_aware: false,
            _subscriber: PhantomData,
        }
    }

    /// Returns true if async context is available and this layer should skip.
    fn should_skip_for_span<S2: Subscriber + for<'a> LookupSpan<'a>>(
        &self,
        id: &span::Id,
        ctx: &Context<'_, S2>,
    ) -> bool {
        if !self.async_aware {
            return false;
        }

        let span = match ctx.span(id) {
            Some(s) => s,
            None => return false,
        };

        // Check if we already recorded the decision for this span
        let extensions = span.extensions();
        if let Some(marker) = extensions.get::<SyncSkipMarker>() {
            return marker.skip;
        }
        drop(extensions);

        // First time: probe whether async context is available and store the result
        let skip = dcontext::async_ctx::current_depth().is_some();
        let mut extensions = span.extensions_mut();
        extensions.insert(SyncSkipMarker { skip });
        skip
    }
}

impl<S> Default for SyncDcontextLayer<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for configuring a [`SyncDcontextLayer`].
pub struct SyncDcontextLayerBuilder<S> {
    include_span_info: bool,
    async_aware: bool,
    _subscriber: PhantomData<fn(S)>,
}

impl<S> SyncDcontextLayerBuilder<S> {
    /// Include span metadata (name, target, level) as a [`SpanInfo`](crate::SpanInfo) context value.
    ///
    /// When enabled, each span enter will set a [`SpanInfo`] value under
    /// the key `"dcontext.span"`.
    pub fn include_span_info(mut self) -> Self {
        self.include_span_info = true;
        self
    }

    /// Make this layer async-aware: when an async task-local context is
    /// available, the sync layer becomes a no-op, deferring all work to
    /// the `AsyncDcontextLayer`. This allows mounting both layers together
    /// so that async code uses the async layer and sync code uses this layer.
    pub fn async_aware(mut self) -> Self {
        self.async_aware = true;
        self
    }

    /// Build the configured [`SyncDcontextLayer`].
    pub fn build(self) -> SyncDcontextLayer<S> {
        SyncDcontextLayer {
            include_span_info: self.include_span_info,
            async_aware: self.async_aware,
            _subscriber: PhantomData,
        }
    }
}

impl<S> Layer<S> for SyncDcontextLayer<S>
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
        // If async-aware and async context is available, skip — async layer handles it.
        if self.should_skip_for_span(id, &ctx) {
            return;
        }

        dcontext::force_thread_local(|| {
            // Level 1: Create a new named dcontext scope
            let guard = if let Some(span) = ctx.span(id) {
                let name = span.metadata().name();
                dcontext::enter_named_scope(name)
            } else {
                dcontext::enter_scope()
            };

            // Level 2: Apply metadata-driven extraction
            if let Some(span) = ctx.span(id) {
                layer_common::apply_field_extraction(&span);
            }

            // Level 3: Set span info
            if self.include_span_info {
                if let Some(span) = ctx.span(id) {
                    layer_common::set_span_info(&span);
                }
            }

            // Level 4: Record context values into span fields
            if let Some(span) = ctx.span(id) {
                layer_common::record_context_to_span(&span);
            }

            guard_stack::push_guard(id, guard);
        });
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        // Check the stored marker — if async context was active, skip.
        if let Some(span) = ctx.span(id) {
            let extensions = span.extensions();
            if let Some(marker) = extensions.get::<SyncSkipMarker>() {
                if marker.skip {
                    return;
                }
            }
        }

        dcontext::force_thread_local(|| {
            guard_stack::pop_guard(id);
        });
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        // Check the stored marker — if async context was active, skip.
        if let Some(span) = ctx.span(&id) {
            let extensions = span.extensions();
            if let Some(marker) = extensions.get::<SyncSkipMarker>() {
                if marker.skip {
                    return;
                }
            }
        }

        dcontext::force_thread_local(|| {
            guard_stack::pop_guard(&id);
        });

        if let Some(span) = ctx.span(&id) {
            let mut extensions = span.extensions_mut();
            extensions.remove::<ExtractedFields>();
            extensions.remove::<SyncSkipMarker>();
        }
    }
}
