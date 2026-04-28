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

/// A [`tracing_subscriber::Layer`] that automatically creates dcontext scopes
/// when tracing spans are entered.
///
/// # Levels of Integration
///
/// 1. **Auto-scoping** (zero config): Every span enter creates a new dcontext
///    scope that inherits the parent scope's values. The scope is reverted on
///    span exit.
///
/// 2. **Field extraction**: Extract tracing span fields into dcontext values.
///    Configure via [`TracingField`](crate::TracingField) metadata at
///    registration time.
///
/// 3. **Span info**: Optionally expose span metadata (name, target, level) as
///    a [`SpanInfo`] context value.
///
/// # Example
///
/// ```ignore
/// use tracing_subscriber::prelude::*;
/// use dcontext_tracing::TracingField;
///
/// // Configure extraction + enrichment at registration time:
/// builder.register_with::<String>("request_id", |opts| {
///     opts.cached().with_metadata(
///         TracingField::builder("rid")
///             .extract_from_str(|s| Some(s.to_string()))
///             .enrich_display::<String>()
///             .build()
///     )
/// });
///
/// // The layer auto-discovers metadata — no field configuration needed:
/// tracing_subscriber::registry()
///     .with(dcontext_tracing::DcontextLayer::new())
///     .init();
/// ```
pub struct DcontextLayer<S> {
    include_span_info: bool,
    _subscriber: PhantomData<fn(S)>,
}

impl<S> DcontextLayer<S> {
    /// Create a new `DcontextLayer` with default settings (auto-scoping only).
    ///
    /// Field extraction is auto-discovered from [`TracingField`](crate::TracingField)
    /// metadata in the registry. No explicit field configuration is needed.
    pub fn new() -> Self {
        Self {
            include_span_info: false,
            _subscriber: PhantomData,
        }
    }

    /// Create a [`DcontextLayerBuilder`] for configuring the layer.
    pub fn builder() -> DcontextLayerBuilder<S> {
        DcontextLayerBuilder {
            include_span_info: false,
            _subscriber: PhantomData,
        }
    }
}

impl<S> Default for DcontextLayer<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for configuring a [`DcontextLayer`].
pub struct DcontextLayerBuilder<S> {
    include_span_info: bool,
    _subscriber: PhantomData<fn(S)>,
}

impl<S> DcontextLayerBuilder<S> {
    /// Include span metadata (name, target, level) as a [`SpanInfo`] context value.
    ///
    /// When enabled, each span enter will set a [`SpanInfo`] value under
    /// the key `"dcontext.span"`.
    pub fn include_span_info(mut self) -> Self {
        self.include_span_info = true;
        self
    }

    /// Build the configured [`DcontextLayer`].
    pub fn build(self) -> DcontextLayer<S> {
        DcontextLayer {
            include_span_info: self.include_span_info,
            _subscriber: PhantomData,
        }
    }
}

impl<S> Layer<S> for DcontextLayer<S>
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

        // Collect fields to extract AND record_field names to track user-set fields
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

        // Extract target fields from span attributes
        let all_target: Vec<&'static str> = extract_names
            .iter()
            .chain(record_fields.iter())
            .copied()
            .collect();
        let mut extractor = FieldExtractor::new(&all_target);
        attrs.record(&mut extractor);

        // Mark any record_field that was explicitly set by the user as user-set.
        // These should NOT be overwritten by auto-record.
        for &rf in &record_fields {
            if extractor.extracted.string_values.contains_key(rf)
                || extractor.extracted.u64_values.contains_key(rf)
                || extractor.extracted.i64_values.contains_key(rf)
                || extractor.extracted.bool_values.contains_key(rf)
            {
                extractor.extracted.mark_user_set(rf);
            }
        }

        // Only keep extraction fields in extracted (remove record-only fields)
        // Actually we keep all — the extraction logic only looks at extract_names
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
        // Skip processing if this on_record was triggered by our own auto-recording
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

            // Mark user-set record fields
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
            // Inside on_enter, Span::current() returns the span being entered.
            // span.record() silently skips fields not declared on the span.
            // We skip fields that were explicitly set by user code.
            if metadata_fields.iter().any(|e| e.span_fmt_fn.is_some()) {
                // Check which fields are user-set (should not be overwritten)
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
                        // Skip fields explicitly set by user code
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
                    // Set flag to prevent on_record from processing our own writes
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

    fn on_exit(&self, id: &span::Id, _ctx: Context<'_, S>) {
        dcontext::force_thread_local(|| {
            guard_stack::pop_guard(id);
        });
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        dcontext::force_thread_local(|| {
            guard_stack::pop_guard(&id);
        });

        if let Some(span) = ctx.span(&id) {
            let mut extensions = span.extensions_mut();
            extensions.remove::<ExtractedFields>();
        }
    }
}
