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

use std::collections::HashSet;
use std::marker::PhantomData;

use tracing::Subscriber;
use tracing_core::span;
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use crate::field_mapping::ExtractedFields;
use crate::guard_stack;
use crate::layer_common;
use crate::span_info::{SpanInfo, SPAN_INFO_KEY};
use crate::tracing_field::get_tracing_fields;

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
    async_aware: bool,
    _subscriber: PhantomData<fn(S)>,
}

impl<S> SyncDcontextLayer<S> {
    /// Create a new `SyncDcontextLayer` with default settings (auto-scoping only).
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

        let extensions = span.extensions();
        if let Some(marker) = extensions.get::<SyncSkipMarker>() {
            return marker.skip;
        }
        drop(extensions);

        let skip = dcontext::async_ctx::is_active();
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
    pub fn include_span_info(mut self) -> Self {
        self.include_span_info = true;
        self
    }

    /// Make this layer async-aware: skip when task-local context is available.
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

// ── Store-specific helpers (sync_ctx) ──────────────────────────

use tracing_subscriber::registry::SpanRef;

/// Apply field extraction from span extensions into the thread-local context.
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
                dcontext::sync_ctx::set_raw_value(entry.context_key, val);
            }
        }
    }
}

/// Set span info in the thread-local context.
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
    dcontext::sync_ctx::set_context(SPAN_INFO_KEY, info);
}

/// Record context values from thread-local store into span fields.
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
            let formatted = dcontext::sync_ctx::with_context_value(entry.context_key, |any_val| {
                fmt_fn(any_val)
            })
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

impl<S> Layer<S> for SyncDcontextLayer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            layer_common::extract_span_fields(attrs, &span);
        }
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            layer_common::merge_recorded_fields(values, &span);
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        if self.should_skip_for_span(id, &ctx) {
            return;
        }

        // Level 1: Create a new named dcontext scope
        let guard = if let Some(span) = ctx.span(id) {
            let name = span.metadata().name();
            dcontext::sync_ctx::push_scope(name)
        } else {
            dcontext::sync_ctx::push_scope("")
        };

        // Level 2: Apply field extraction
        if let Some(span) = ctx.span(id) {
            apply_field_extraction(&span);
        }

        // Level 3: Set span info
        if self.include_span_info {
            if let Some(span) = ctx.span(id) {
                set_span_info(&span);
            }
        }

        // Level 4: Record context values into span fields
        if let Some(span) = ctx.span(id) {
            record_context_to_span(&span);
        }

        guard_stack::push_guard(id, guard);
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let extensions = span.extensions();
            if let Some(marker) = extensions.get::<SyncSkipMarker>() {
                if marker.skip {
                    return;
                }
            }
        }

        guard_stack::pop_guard(id);
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(&id) {
            let extensions = span.extensions();
            if let Some(marker) = extensions.get::<SyncSkipMarker>() {
                if marker.skip {
                    return;
                }
            }
        }

        guard_stack::pop_guard(&id);

        if let Some(span) = ctx.span(&id) {
            let mut extensions = span.extensions_mut();
            extensions.remove::<ExtractedFields>();
            extensions.remove::<SyncSkipMarker>();
        }
    }
}
