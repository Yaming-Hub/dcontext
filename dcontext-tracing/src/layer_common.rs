//! Shared logic for AsyncDcontextLayer and SyncDcontextLayer.
//!
//! These helpers handle field extraction into span extensions and span recording —
//! the parts that are identical between async and sync layers and do NOT access
//! the context store directly.
//!
//! Functions that read/write the context store (apply_field_extraction,
//! set_span_info, record_context_to_span) are defined separately in each layer
//! since they target different stores.

use std::cell::Cell;

use tracing_subscriber::registry::{LookupSpan, SpanRef};

use crate::field_mapping::{ExtractedFields, FieldExtractor};
use crate::tracing_field::get_tracing_fields;

// Flag to prevent on_record from processing values recorded by our own layer.
thread_local! {
    pub(crate) static SELF_RECORDING: Cell<bool> = const { Cell::new(false) };
}

/// Extract fields from span attributes and store in span extensions.
/// Called from `on_new_span` in both layers.
pub(crate) fn extract_span_fields<S>(attrs: &tracing_core::span::Attributes<'_>, span: &SpanRef<'_, S>)
where
    S: for<'a> LookupSpan<'a>,
{
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

/// Merge late-recorded field values into span extensions.
/// Called from `on_record` in both layers.
pub(crate) fn merge_recorded_fields<S>(values: &tracing_core::span::Record<'_>, span: &SpanRef<'_, S>)
where
    S: for<'a> LookupSpan<'a>,
{
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
