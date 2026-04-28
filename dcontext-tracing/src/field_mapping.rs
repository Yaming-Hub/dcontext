use std::collections::{HashMap, HashSet};
use std::fmt;

use tracing_core::field::{Field, Visit};

/// Extracted field values from a span, stored in span extensions.
///
/// Must be `Send + Sync` to satisfy span extension requirements.
pub(crate) struct ExtractedFields {
    pub string_values: HashMap<&'static str, String>,
    pub u64_values: HashMap<&'static str, u64>,
    pub i64_values: HashMap<&'static str, i64>,
    pub bool_values: HashMap<&'static str, bool>,
    /// Fields that were explicitly set by user code (not auto-recorded).
    /// Auto-record should not overwrite these.
    pub user_set_fields: HashSet<&'static str>,
}

impl ExtractedFields {
    pub fn new() -> Self {
        Self {
            string_values: HashMap::new(),
            u64_values: HashMap::new(),
            i64_values: HashMap::new(),
            bool_values: HashMap::new(),
            user_set_fields: HashSet::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.string_values.is_empty()
            && self.u64_values.is_empty()
            && self.i64_values.is_empty()
            && self.bool_values.is_empty()
    }

    /// Mark a field as user-set (not from auto-record).
    pub fn mark_user_set(&mut self, field: &'static str) {
        self.user_set_fields.insert(field);
    }
}

/// Visitor that extracts field values from span attributes.
pub(crate) struct FieldExtractor<'a> {
    /// Set of field names we're interested in.
    pub target_fields: &'a [&'static str],
    pub extracted: ExtractedFields,
}

impl<'a> FieldExtractor<'a> {
    pub fn new(target_fields: &'a [&'static str]) -> Self {
        Self {
            target_fields,
            extracted: ExtractedFields::new(),
        }
    }

    fn is_target(&self, field: &Field) -> Option<&'static str> {
        self.target_fields.iter().find(|&&f| f == field.name()).copied()
    }
}

impl<'a> Visit for FieldExtractor<'a> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if let Some(key) = self.is_target(field) {
            self.extracted.string_values.insert(key, value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if let Some(key) = self.is_target(field) {
            // Fall back to Debug formatting for non-string types
            if !self.extracted.string_values.contains_key(key)
                && !self.extracted.u64_values.contains_key(key)
                && !self.extracted.i64_values.contains_key(key)
                && !self.extracted.bool_values.contains_key(key)
            {
                self.extracted
                    .string_values
                    .insert(key, format!("{:?}", value));
            }
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if let Some(key) = self.is_target(field) {
            self.extracted.u64_values.insert(key, value);
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if let Some(key) = self.is_target(field) {
            self.extracted.i64_values.insert(key, value);
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if let Some(key) = self.is_target(field) {
            self.extracted.bool_values.insert(key, value);
        }
    }
}
