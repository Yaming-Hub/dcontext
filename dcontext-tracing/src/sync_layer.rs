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

use std::cell::Cell;
use std::marker::PhantomData;

use tracing::Subscriber;
use tracing_core::span;
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use crate::field_mapping::{ExtractedFields, FieldExtractor};
use crate::guard_stack;
use crate::span_info::{SpanInfo, SPAN_INFO_KEY};
use crate::tracing_field::get_tracing_fields;

// Flag to prevent on_record from processing values recorded by our own layer.
thread_local! {
    static SELF_RECORDING: Cell<bool> = const { Cell::new(false) };
}

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
        let metadata_fields = get_tracing_fields();

        let extract_names: Vec<&'static str> = metadata_fields
            .iter()
            .filter(|e| e.extract.is_some())
            .map(|e| e.span_field)
            .collect();

        let record_fields: Vec<&'static str> = metadata_fields
            .iter()
            .filter(|e| e.span_fmt_fn.is_some())
            .map(|e| e.record_field)
            .collect();

        if extract_names.is_empty() && record_fields.is_empty() {
            return;
        }

        let span = match ctx.span(id) {
            Some(s) => s,
            None => return,
        };

        let all_target: Vec<&'static str> = extract_names
            .iter()
            .chain(record_fields.iter())
            .copied()
            .collect();
        let mut extractor = FieldExtractor::new(&all_target);
        attrs.record(&mut extractor);

        for &rf in &record_fields {
            if extractor.extracted.string_values.contains_key(rf)
                || extractor.extracted.u64_values.contains_key(rf)
                || extractor.extracted.i64_values.contains_key(rf)
                || extractor.extracted.bool_values.contains_key(rf)
            {
                extractor.extracted.mark_user_set(rf);
            }
        }

        if !extractor.extracted.is_empty() || !extractor.extracted.user_set_fields.is_empty() {
            let mut extensions = span.extensions_mut();
            extensions.insert(extractor.extracted);
        }
    }

    fn on_record(
        &self,
        id: &span::Id,
        values: &span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        if SELF_RECORDING.with(|f| f.get()) {
            return;
        }

        let metadata_fields = get_tracing_fields();
        let extract_names: Vec<&'static str> = metadata_fields
            .iter()
            .filter(|e| e.extract.is_some())
            .map(|e| e.span_field)
            .collect();

        let record_fields: Vec<&'static str> = metadata_fields
            .iter()
            .filter(|e| e.span_fmt_fn.is_some())
            .map(|e| e.record_field)
            .collect();

        if extract_names.is_empty() && record_fields.is_empty() {
            return;
        }

        let span = match ctx.span(id) {
            Some(s) => s,
            None => return,
        };

        let all_target: Vec<&'static str> = extract_names
            .iter()
            .chain(record_fields.iter())
            .copied()
            .collect();
        let mut extractor = FieldExtractor::new(&all_target);
        values.record(&mut extractor);

        if !extractor.extracted.is_empty() {
            let mut extensions = span.extensions_mut();

            for &rf in &record_fields {
                if extractor.extracted.string_values.contains_key(rf)
                    || extractor.extracted.u64_values.contains_key(rf)
                    || extractor.extracted.i64_values.contains_key(rf)
                    || extractor.extracted.bool_values.contains_key(rf)
                {
                    extractor.extracted.mark_user_set(rf);
                }
            }

            if let Some(existing) = extensions.get_mut::<ExtractedFields>() {
                existing.string_values.extend(extractor.extracted.string_values);
                existing.u64_values.extend(extractor.extracted.u64_values);
                existing.i64_values.extend(extractor.extracted.i64_values);
                existing.bool_values.extend(extractor.extracted.bool_values);
                existing.user_set_fields.extend(extractor.extracted.user_set_fields);
            } else {
                extensions.insert(extractor.extracted);
            }
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
            let metadata_fields = get_tracing_fields();
            if metadata_fields.iter().any(|e| e.extract.is_some()) {
                if let Some(span) = ctx.span(id) {
                    let extensions = span.extensions();
                    if let Some(fields) = extensions.get::<ExtractedFields>() {
                        for entry in metadata_fields {
                            if let Some(ref extract) = entry.extract {
                                if let Some(v) = fields.string_values.get(entry.span_field) {
                                    if let Some(ref f) = extract.from_str {
                                        f(v, entry.context_key);
                                    }
                                } else if let Some(&v) = fields.u64_values.get(entry.span_field) {
                                    if let Some(ref f) = extract.from_u64 {
                                        f(v, entry.context_key);
                                    }
                                } else if let Some(&v) = fields.i64_values.get(entry.span_field) {
                                    if let Some(ref f) = extract.from_i64 {
                                        f(v, entry.context_key);
                                    }
                                } else if let Some(&v) = fields.bool_values.get(entry.span_field) {
                                    if let Some(ref f) = extract.from_bool {
                                        f(v, entry.context_key);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Level 3: Set span info
            if self.include_span_info {
                if let Some(span) = ctx.span(id) {
                    let metadata = span.metadata();
                    let info = SpanInfo {
                        name: metadata.name().to_string(),
                        target: metadata.target().to_string(),
                        level: metadata.level().to_string(),
                    };
                    dcontext::set_context(SPAN_INFO_KEY, info);
                }
            }

            // Level 4: Record context values into span fields.
            if metadata_fields.iter().any(|e| e.span_fmt_fn.is_some()) {
                let user_set: std::collections::HashSet<&str> = ctx
                    .span(id)
                    .and_then(|span| {
                        let extensions = span.extensions();
                        extensions
                            .get::<ExtractedFields>()
                            .map(|ef| ef.user_set_fields.iter().copied().collect())
                    })
                    .unwrap_or_default();

                let to_record: Vec<(&'static str, String)> = metadata_fields
                    .iter()
                    .filter_map(|entry| {
                        let fmt_fn = entry.span_fmt_fn.as_ref()?;
                        if user_set.contains(entry.record_field) {
                            return None;
                        }
                        let formatted = dcontext::with_context_value(
                            entry.context_key,
                            |any_val| fmt_fn(any_val),
                        )
                        .flatten()?;
                        Some((entry.record_field, formatted))
                    })
                    .collect();

                if !to_record.is_empty() {
                    let current = tracing::Span::current();
                    SELF_RECORDING.with(|f| f.set(true));
                    for (field_name, value) in &to_record {
                        current.record(*field_name, value.as_str());
                    }
                    SELF_RECORDING.with(|f| f.set(false));
                }
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
