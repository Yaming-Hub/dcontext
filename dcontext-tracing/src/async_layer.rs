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

use std::collections::HashSet;
use std::marker::PhantomData;

use tracing::Subscriber;
use tracing_core::span;
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use crate::field_mapping::ExtractedFields;
use crate::layer_common;
use crate::span_info::{SpanInfo, SPAN_INFO_KEY};
use crate::tracing_field::get_tracing_fields;

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

// ── Store-specific helpers (async_ctx) ─────────────────────────

use tracing_subscriber::registry::SpanRef;

/// Apply field extraction from span extensions into the task-local context.
fn apply_field_extraction<S>(span: &SpanRef<'_, S>)
where
    S: for<'a> LookupSpan<'a>,
{
    let metadata_fields = get_tracing_fields();
    if !metadata_fields.iter().any(|e| e.extract.is_some()) {
        return;
    }

    let extensions = span.extensions();
    let fields = match extensions.get::<ExtractedFields>() {
        Some(f) => f,
        None => return,
    };

    for entry in metadata_fields {
        if let Some(ref extract) = entry.extract {
            let value = if let Some(v) = fields.string_values.get(entry.span_field) {
                extract.from_str.as_ref().and_then(|f| f(v))
            } else if let Some(&v) = fields.u64_values.get(entry.span_field) {
                extract.from_u64.as_ref().and_then(|f| f(v))
            } else if let Some(&v) = fields.i64_values.get(entry.span_field) {
                extract.from_i64.as_ref().and_then(|f| f(v))
            } else if let Some(&v) = fields.bool_values.get(entry.span_field) {
                extract.from_bool.as_ref().and_then(|f| f(v))
            } else {
                None
            };

            if let Some(val) = value {
                dcontext::async_ctx::set_raw_value(entry.context_key, val);
            }
        }
    }
}

/// Set span info in the task-local context.
fn set_span_info<S>(span: &SpanRef<'_, S>)
where
    S: for<'a> LookupSpan<'a>,
{
    let metadata = span.metadata();
    let info = SpanInfo {
        name: metadata.name().to_string(),
        target: metadata.target().to_string(),
        level: metadata.level().to_string(),
    };
    dcontext::async_ctx::set_context(SPAN_INFO_KEY, info);
}

/// Record context values from task-local store into span fields.
fn record_context_to_span<S>(span: &SpanRef<'_, S>)
where
    S: for<'a> LookupSpan<'a>,
{
    let metadata_fields = get_tracing_fields();
    if !metadata_fields.iter().any(|e| e.span_fmt_fn.is_some()) {
        return;
    }

    let user_set: HashSet<&str> = {
        let extensions = span.extensions();
        extensions
            .get::<ExtractedFields>()
            .map(|ef| ef.user_set_fields.iter().copied().collect())
            .unwrap_or_default()
    };

    let to_record: Vec<(&'static str, String)> = metadata_fields
        .iter()
        .filter_map(|entry| {
            let fmt_fn = entry.span_fmt_fn.as_ref()?;
            if user_set.contains(entry.record_field) {
                return None;
            }
            let formatted = dcontext::async_ctx::with_context_value(
                entry.context_key,
                |any_val| fmt_fn(any_val),
            )
            .flatten()?;
            Some((entry.record_field, formatted))
        })
        .collect();

    if !to_record.is_empty() {
        let current = tracing::Span::current();
        layer_common::SELF_RECORDING.with(|f| f.set(true));
        for (field_name, value) in &to_record {
            current.record(*field_name, value.as_str());
        }
        layer_common::SELF_RECORDING.with(|f| f.set(false));
    }
}

// ── Layer implementation ───────────────────────────────────────

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
        {
            let extensions = span.extensions();
            if extensions.get::<AsyncScopeState>().is_some() {
                return;
            }
        }

        // First enter: push scope on task-local store.
        let name = span.metadata().name();
        let guard = match dcontext::async_ctx::try_push_scope(name) {
            Some(guard) => guard,
            None => return,
        };
        let depth = guard.expected_depth();
        std::mem::forget(guard);

        // Level 2: Apply field extraction into task-local context
        apply_field_extraction(&span);

        // Level 3: Set span info
        if self.include_span_info {
            set_span_info(&span);
        }

        // Level 4: Record context values into span fields
        record_context_to_span(&span);

        // Store depth in span extensions for verification on close
        let mut extensions = span.extensions_mut();
        extensions.insert(AsyncScopeState { depth });
    }

    fn on_exit(&self, _id: &span::Id, _ctx: Context<'_, S>) {
        // Intentionally do nothing — scopes persist across yields
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        let span = match ctx.span(&id) {
            Some(s) => s,
            None => return,
        };

        let state = {
            let mut extensions = span.extensions_mut();
            extensions.remove::<AsyncScopeState>()
        };

        if let Some(state) = state {
            let current = dcontext::async_ctx::current_depth();
            if current == Some(state.depth) {
                dcontext::async_ctx::pop_scope(state.depth);
            }
        }

        {
            let mut extensions = span.extensions_mut();
            extensions.remove::<ExtractedFields>();
        }
    }
}

