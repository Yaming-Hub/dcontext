use std::marker::PhantomData;

use tracing::Subscriber;
use tracing_core::span;
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use crate::field_mapping::{
    ExtractedFields, FieldExtractor, FieldMapping, TypedFieldSetter,
};
use crate::guard_stack;
use crate::span_info::{SpanInfo, SPAN_INFO_KEY};
use crate::FromFieldValue;

/// A [`tracing_subscriber::Layer`] that automatically creates dcontext scopes
/// when tracing spans are entered.
///
/// # Levels of Integration
///
/// 1. **Auto-scoping** (zero config): Every span enter creates a new dcontext
///    scope that inherits the parent scope's values. The scope is reverted on
///    span exit.
///
/// 2. **Field mapping**: Map tracing span fields to dcontext keys. When a span
///    with the configured field is entered, the value is extracted and set in
///    the new context scope.
///
/// 3. **Span info**: Optionally expose span metadata (name, target, level) as
///    a [`SpanInfo`] context value.
///
/// # Example
///
/// ```ignore
/// use tracing_subscriber::prelude::*;
///
/// let layer = dcontext_tracing::DcontextLayer::builder()
///     .map_field::<RequestId>("request_id")
///     .include_span_info()
///     .build();
///
/// tracing_subscriber::registry()
///     .with(layer)
///     .init();
/// ```
pub struct DcontextLayer<S> {
    field_mappings: Vec<FieldMapping>,
    include_span_info: bool,
    _subscriber: PhantomData<fn(S)>,
}

impl<S> DcontextLayer<S> {
    /// Create a new `DcontextLayer` with default settings (auto-scoping only).
    pub fn new() -> Self {
        Self {
            field_mappings: Vec::new(),
            include_span_info: false,
            _subscriber: PhantomData,
        }
    }

    /// Create a [`DcontextLayerBuilder`] for configuring the layer.
    pub fn builder() -> DcontextLayerBuilder<S> {
        DcontextLayerBuilder {
            field_mappings: Vec::new(),
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
    field_mappings: Vec<FieldMapping>,
    include_span_info: bool,
    _subscriber: PhantomData<fn(S)>,
}

impl<S> DcontextLayerBuilder<S> {
    /// Map a tracing span field to a dcontext key.
    ///
    /// When a span containing a field with the given name is entered,
    /// the field value is extracted and set as a dcontext value of type `T`.
    ///
    /// The field name is used as both the tracing field name and the
    /// dcontext key.
    ///
    /// # Type Requirements
    ///
    /// `T` must implement [`FromFieldValue`] for conversion from tracing
    /// field values to the context type.
    pub fn map_field<T: FromFieldValue>(mut self, field_name: &'static str) -> Self {
        self.field_mappings.push(FieldMapping {
            field_name,
            context_key: field_name,
            setter: Box::new(TypedFieldSetter::<T>::new()),
        });
        self
    }

    /// Map a tracing span field to a dcontext key with a different key name.
    ///
    /// Similar to [`map_field`](Self::map_field) but allows using a different
    /// name for the dcontext key than the tracing field name.
    pub fn map_field_as<T: FromFieldValue>(
        mut self,
        field_name: &'static str,
        context_key: &'static str,
    ) -> Self {
        self.field_mappings.push(FieldMapping {
            field_name,
            context_key,
            setter: Box::new(TypedFieldSetter::<T>::new()),
        });
        self
    }

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
            field_mappings: self.field_mappings,
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
        if self.field_mappings.is_empty() {
            return;
        }

        let span = match ctx.span(id) {
            Some(s) => s,
            None => return,
        };

        // Extract the field values we're interested in
        let field_names: Vec<&'static str> =
            self.field_mappings.iter().map(|m| m.field_name).collect();
        let mut extractor = FieldExtractor::new(&field_names);
        attrs.record(&mut extractor);

        if !extractor.extracted.is_empty() {
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
        if self.field_mappings.is_empty() {
            return;
        }

        let span = match ctx.span(id) {
            Some(s) => s,
            None => return,
        };

        let field_names: Vec<&'static str> =
            self.field_mappings.iter().map(|m| m.field_name).collect();
        let mut extractor = FieldExtractor::new(&field_names);
        values.record(&mut extractor);

        if !extractor.extracted.is_empty() {
            let mut extensions = span.extensions_mut();
            if let Some(existing) = extensions.get_mut::<ExtractedFields>() {
                // Merge new values into existing extracted fields
                existing.string_values.extend(extractor.extracted.string_values);
                existing.u64_values.extend(extractor.extracted.u64_values);
                existing.i64_values.extend(extractor.extracted.i64_values);
                existing.bool_values.extend(extractor.extracted.bool_values);
            } else {
                extensions.insert(extractor.extracted);
            }
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        // Use force_thread_local to ensure dcontext uses thread-local storage.
        // This is necessary because on_enter/on_exit are synchronous callbacks
        // that may be called inside a tokio runtime (e.g., via Instrument).
        dcontext::force_thread_local(|| {
            // Level 1: Create a new dcontext scope
            let guard = dcontext::enter_scope();

            // Level 2: Apply field mappings from extracted values
            if !self.field_mappings.is_empty() {
                if let Some(span) = ctx.span(id) {
                    let extensions = span.extensions();
                    if let Some(fields) = extensions.get::<ExtractedFields>() {
                        for mapping in &self.field_mappings {
                            // Try each value type in order of specificity
                            if let Some(v) = fields.string_values.get(mapping.field_name) {
                                mapping.setter.set_from_str(mapping.context_key, v);
                            } else if let Some(&v) = fields.u64_values.get(mapping.field_name) {
                                mapping.setter.set_from_u64(mapping.context_key, v);
                            } else if let Some(&v) = fields.i64_values.get(mapping.field_name) {
                                mapping.setter.set_from_i64(mapping.context_key, v);
                            } else if let Some(&v) = fields.bool_values.get(mapping.field_name) {
                                mapping.setter.set_from_bool(mapping.context_key, v);
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

            // Store the guard in the thread-local stack
            guard_stack::push_guard(id, guard);
        });
    }

    fn on_exit(&self, id: &span::Id, _ctx: Context<'_, S>) {
        // Pop the guard inside force_thread_local so the drop (leave_scope)
        // also uses thread-local storage.
        dcontext::force_thread_local(|| {
            guard_stack::pop_guard(id);
        });
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        // Clean up any stale guards (defensive: handles missed on_exit)
        dcontext::force_thread_local(|| {
            guard_stack::pop_guard(&id);
        });

        // Clean up extracted fields from extensions
        if let Some(span) = ctx.span(&id) {
            let mut extensions = span.extensions_mut();
            extensions.remove::<ExtractedFields>();
        }
    }
}
