//! Shared logic for AsyncDcontextLayer and SyncDcontextLayer.
//!
//! These helpers handle field extraction, span recording, and span info —
//! the parts that are identical between async and sync layers.

use std::cell::Cell;
use std::collections::HashSet;

use tracing_subscriber::registry::{LookupSpan, SpanRef};

use crate::field_mapping::{ExtractedFields, FieldExtractor};
use crate::span_info::{SpanInfo, SPAN_INFO_KEY};
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

/// Apply field extraction from span extensions into the active context store.
/// The extraction functions call `dcontext::set_context` which dispatches to
/// the appropriate store (task-local or thread-local) based on runtime context.
/// Called from `on_enter` in both layers (Level 2).
pub(crate) fn apply_field_extraction<S>(span: &SpanRef<'_, S>)
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

/// Set span info (name, target, level) in the context.
/// Uses `dcontext::set_context` which dispatches appropriately.
/// Called from `on_enter` in both layers (Level 3).
pub(crate) fn set_span_info<S>(span: &SpanRef<'_, S>)
where
    S: for<'a> LookupSpan<'a>,
{
    let metadata = span.metadata();
    let info = SpanInfo {
        name: metadata.name().to_string(),
        target: metadata.target().to_string(),
        level: metadata.level().to_string(),
    };
    dcontext::set_context(SPAN_INFO_KEY, info);
}

/// Record context values into span fields (auto-fill Empty fields).
/// Skips fields that were explicitly set by user code.
/// Called from `on_enter` in both layers (Level 4).
pub(crate) fn record_context_to_span<S>(span: &SpanRef<'_, S>)
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
