use std::collections::HashMap;
use std::fmt;

use tracing_core::field::{Field, Visit};

/// A mapping from a tracing span field to a dcontext key.
///
/// When a span with the configured field is entered, the field value
/// is extracted and set as a dcontext context value.
pub(crate) struct FieldMapping {
    pub field_name: &'static str,
    pub context_key: &'static str,
    pub setter: Box<dyn FieldSetter>,
}

/// Trait for setting a dcontext value from a tracing field value.
pub(crate) trait FieldSetter: Send + Sync {
    /// Try to set the context value from a string representation.
    fn set_from_str(&self, key: &'static str, value: &str);
    /// Try to set the context value from a u64.
    fn set_from_u64(&self, key: &'static str, value: u64);
    /// Try to set the context value from an i64.
    fn set_from_i64(&self, key: &'static str, value: i64);
    /// Try to set the context value from a bool.
    fn set_from_bool(&self, key: &'static str, value: bool);
}

/// Trait for types that can be constructed from tracing field values.
///
/// Implement this for your context types to enable automatic
/// field-to-context mapping.
///
/// # Example
///
/// ```rust
/// use dcontext_tracing::FromFieldValue;
///
/// #[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
/// struct RequestId(String);
///
/// impl FromFieldValue for RequestId {
///     fn from_str_value(s: &str) -> Option<Self> {
///         Some(RequestId(s.to_string()))
///     }
/// }
/// ```
pub trait FromFieldValue: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static {
    /// Construct from a string value. Returns `None` if conversion fails.
    fn from_str_value(s: &str) -> Option<Self> {
        let _ = s;
        None
    }

    /// Construct from a u64 value. Returns `None` if conversion fails.
    fn from_u64_value(v: u64) -> Option<Self> {
        let _ = v;
        None
    }

    /// Construct from an i64 value. Returns `None` if conversion fails.
    fn from_i64_value(v: i64) -> Option<Self> {
        let _ = v;
        None
    }

    /// Construct from a bool value. Returns `None` if conversion fails.
    fn from_bool_value(v: bool) -> Option<Self> {
        let _ = v;
        None
    }
}

/// Built-in implementation for `String`, allowing direct field-to-context
/// mapping without a newtype wrapper:
///
/// ```rust,no_run
/// # use dcontext_tracing::DcontextLayer;
/// # use tracing_subscriber::Registry;
/// let layer: DcontextLayer<Registry> = DcontextLayer::builder()
///     .map_field::<String>("job_id")
///     .build();
/// ```
impl FromFieldValue for String {
    fn from_str_value(s: &str) -> Option<Self> {
        Some(s.to_string())
    }
}

/// A concrete FieldSetter for a specific type T.
pub(crate) struct TypedFieldSetter<T> {
    _marker: std::marker::PhantomData<T>,
}

impl<T> TypedFieldSetter<T> {
    pub fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> FieldSetter for TypedFieldSetter<T>
where
    T: FromFieldValue,
{
    fn set_from_str(&self, key: &'static str, value: &str) {
        if let Some(v) = T::from_str_value(value) {
            dcontext::set_context(key, v);
        }
    }

    fn set_from_u64(&self, key: &'static str, value: u64) {
        if let Some(v) = T::from_u64_value(value) {
            dcontext::set_context(key, v);
        }
    }

    fn set_from_i64(&self, key: &'static str, value: i64) {
        if let Some(v) = T::from_i64_value(value) {
            dcontext::set_context(key, v);
        }
    }

    fn set_from_bool(&self, key: &'static str, value: bool) {
        if let Some(v) = T::from_bool_value(value) {
            dcontext::set_context(key, v);
        }
    }
}

/// Extracted field values from a span, stored in span extensions.
///
/// Must be `Send + Sync` to satisfy span extension requirements.
pub(crate) struct ExtractedFields {
    pub string_values: HashMap<&'static str, String>,
    pub u64_values: HashMap<&'static str, u64>,
    pub i64_values: HashMap<&'static str, i64>,
    pub bool_values: HashMap<&'static str, bool>,
}

impl ExtractedFields {
    pub fn new() -> Self {
        Self {
            string_values: HashMap::new(),
            u64_values: HashMap::new(),
            i64_values: HashMap::new(),
            bool_values: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.string_values.is_empty()
            && self.u64_values.is_empty()
            && self.i64_values.is_empty()
            && self.bool_values.is_empty()
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
