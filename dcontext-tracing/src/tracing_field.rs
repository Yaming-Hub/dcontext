//! Unified tracing metadata for context-to-log, context-to-span, and span-to-context integration.
//!
//! [`TracingField`] is the single metadata type that controls all three directions:
//!
//! - **Extract** (span field → context): When a tracing span contains a matching
//!   field, the value is extracted and set as a context variable.
//! - **Enrich log** (context → log event): The current context value is formatted
//!   and included in every log event via [`WithContextFields`](crate::WithContextFields).
//! - **Record span** (context → span field): The current context value is recorded
//!   into pre-declared span fields on span enter.
//!
//! All behaviors are opt-in per key, configured at registration time.

use std::any::Any;
use std::fmt;
use std::sync::{Arc, OnceLock};

// ── TracingField metadata ──────────────────────────────────────

/// Unified tracing metadata for a context key.
///
/// Controls three behaviors:
/// - **Extract**: populate a context variable from a tracing span field
/// - **Enrich log**: include the context value in log output
/// - **Record span**: record the context value into span fields
///
/// Created via [`TracingFieldBuilder`]:
///
/// ```rust,ignore
/// use dcontext_tracing::TracingField;
///
/// builder.register_with::<String>("request_id", |opts| {
///     opts.cached().with_metadata(
///         TracingField::builder("rid")
///             .extract_from_str(|s| Some(s.to_string()))
///             .enrich_display::<String>()  // enables both log + span
///             .build()
///     )
/// });
/// ```
pub struct TracingField {
    /// Field name for log output and span recording.
    log_name: &'static str,
    /// Span field name to extract from. If `None`, uses the context key.
    span_field: Option<&'static str>,
    /// Span field name to record into. If `None`, uses `log_name`.
    record_field: Option<&'static str>,
    /// Extract closures: each calls `dcontext::set_context` internally.
    extract: Option<ExtractFns>,
    /// Format function for log enrichment (context → log event).
    log_fmt_fn: Option<Arc<dyn Fn(&dyn Any) -> Option<String> + Send + Sync>>,
    /// Format function for span recording (context → span field).
    span_fmt_fn: Option<Arc<dyn Fn(&dyn Any) -> Option<String> + Send + Sync>>,
}

/// Closures for extracting tracing field values into context.
///
/// Each closure takes (field_value, context_key) and calls `set_context`
/// internally, capturing the concrete type T.
pub(crate) struct ExtractFns {
    pub from_str: Option<Arc<dyn Fn(&str, &'static str) + Send + Sync>>,
    pub from_u64: Option<Arc<dyn Fn(u64, &'static str) + Send + Sync>>,
    pub from_i64: Option<Arc<dyn Fn(i64, &'static str) + Send + Sync>>,
    pub from_bool: Option<Arc<dyn Fn(bool, &'static str) + Send + Sync>>,
}

impl TracingField {
    /// Start building a `TracingField` with the given log field name.
    ///
    /// The `log_name` is the field name used in log output and (by default)
    /// span recording. It can differ from the context key.
    pub fn builder(log_name: &'static str) -> TracingFieldBuilder {
        TracingFieldBuilder {
            log_name,
            span_field: None,
            record_field: None,
            from_str: None,
            from_u64: None,
            from_i64: None,
            from_bool: None,
            log_fmt_fn: None,
            span_fmt_fn: None,
        }
    }

    /// The field name used in log output.
    pub fn log_name(&self) -> &'static str {
        self.log_name
    }

    /// The span field name to extract from, if extraction is enabled.
    pub fn span_field(&self) -> Option<&'static str> {
        self.span_field
    }

    /// The span field name to record into.
    /// Returns `record_field` if set, otherwise `log_name`.
    pub fn record_field(&self) -> &'static str {
        self.record_field.unwrap_or(self.log_name)
    }

    /// Whether this field has extraction enabled.
    pub fn has_extract(&self) -> bool {
        self.extract.is_some()
    }

    /// Whether this field has any enrichment enabled (log or span).
    pub fn has_enrich(&self) -> bool {
        self.log_fmt_fn.is_some() || self.span_fmt_fn.is_some()
    }

    /// Whether this field has log enrichment enabled.
    pub fn has_log_enrich(&self) -> bool {
        self.log_fmt_fn.is_some()
    }

    /// Whether this field has span recording enabled.
    pub fn has_span_record(&self) -> bool {
        self.span_fmt_fn.is_some()
    }

    /// Format a type-erased value for log enrichment.
    /// Returns `None` if log enrichment is not enabled or the type doesn't match.
    pub fn format(&self, any_val: &dyn Any) -> Option<String> {
        self.log_fmt_fn.as_ref().and_then(|f| f(any_val))
    }

    /// Format a type-erased value for span recording.
    /// Returns `None` if span recording is not enabled or the type doesn't match.
    pub fn format_for_span(&self, any_val: &dyn Any) -> Option<String> {
        self.span_fmt_fn.as_ref().and_then(|f| f(any_val))
    }
}

// ── TracingFieldBuilder ────────────────────────────────────────

/// Builder for [`TracingField`] metadata.
pub struct TracingFieldBuilder {
    log_name: &'static str,
    span_field: Option<&'static str>,
    record_field: Option<&'static str>,
    from_str: Option<Arc<dyn Fn(&str, &'static str) + Send + Sync>>,
    from_u64: Option<Arc<dyn Fn(u64, &'static str) + Send + Sync>>,
    from_i64: Option<Arc<dyn Fn(i64, &'static str) + Send + Sync>>,
    from_bool: Option<Arc<dyn Fn(bool, &'static str) + Send + Sync>>,
    log_fmt_fn: Option<Arc<dyn Fn(&dyn Any) -> Option<String> + Send + Sync>>,
    span_fmt_fn: Option<Arc<dyn Fn(&dyn Any) -> Option<String> + Send + Sync>>,
}

impl TracingFieldBuilder {
    /// Set the span field name to extract from.
    ///
    /// If not set, the context key (from registration) is used as the
    /// span field name for extraction.
    pub fn span_field(mut self, name: &'static str) -> Self {
        self.span_field = Some(name);
        self
    }

    /// Set the span field name to record into.
    ///
    /// If not set, `log_name` is used as the span field name for recording.
    /// Use this when the span field you want to record into differs from
    /// the log output field name.
    pub fn record_as(mut self, name: &'static str) -> Self {
        self.record_field = Some(name);
        self
    }

    // ── Extract (span → context) ───────────────────────────────

    /// Extract from string span field values.
    ///
    /// The closure receives the string value and returns `Some(T)` if
    /// conversion succeeds. `set_context` is called automatically.
    pub fn extract_from_str<T>(mut self, f: impl Fn(&str) -> Option<T> + Send + Sync + 'static) -> Self
    where
        T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        self.from_str = Some(Arc::new(move |value, key| {
            if let Some(v) = f(value) {
                dcontext::set_context(key, v);
            }
        }));
        self
    }

    /// Extract from u64 span field values.
    pub fn extract_from_u64<T>(mut self, f: impl Fn(u64) -> Option<T> + Send + Sync + 'static) -> Self
    where
        T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        self.from_u64 = Some(Arc::new(move |value, key| {
            if let Some(v) = f(value) {
                dcontext::set_context(key, v);
            }
        }));
        self
    }

    /// Extract from i64 span field values.
    pub fn extract_from_i64<T>(mut self, f: impl Fn(i64) -> Option<T> + Send + Sync + 'static) -> Self
    where
        T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        self.from_i64 = Some(Arc::new(move |value, key| {
            if let Some(v) = f(value) {
                dcontext::set_context(key, v);
            }
        }));
        self
    }

    /// Extract from bool span field values.
    pub fn extract_from_bool<T>(mut self, f: impl Fn(bool) -> Option<T> + Send + Sync + 'static) -> Self
    where
        T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        self.from_bool = Some(Arc::new(move |value, key| {
            if let Some(v) = f(value) {
                dcontext::set_context(key, v);
            }
        }));
        self
    }

    // ── Enrich: shorthand (enables BOTH log + span) ────────────

    /// Enrich both log output and span fields using [`Display`](std::fmt::Display).
    ///
    /// This is shorthand for calling both `.enrich_log_display::<T>()`
    /// and `.enrich_span_display::<T>()`.
    pub fn enrich_display<T: fmt::Display + 'static>(self) -> Self {
        let fmt: Arc<dyn Fn(&dyn Any) -> Option<String> + Send + Sync> =
            Arc::new(|any_val| any_val.downcast_ref::<T>().map(|v| v.to_string()));
        Self {
            log_fmt_fn: Some(Arc::clone(&fmt)),
            span_fmt_fn: Some(fmt),
            ..self
        }
    }

    /// Enrich both log output and span fields using [`Debug`](std::fmt::Debug).
    ///
    /// This is shorthand for calling both `.enrich_log_debug::<T>()`
    /// and `.enrich_span_debug::<T>()`.
    pub fn enrich_debug<T: fmt::Debug + 'static>(self) -> Self {
        let fmt: Arc<dyn Fn(&dyn Any) -> Option<String> + Send + Sync> =
            Arc::new(|any_val| any_val.downcast_ref::<T>().map(|v| format!("{:?}", v)));
        Self {
            log_fmt_fn: Some(Arc::clone(&fmt)),
            span_fmt_fn: Some(fmt),
            ..self
        }
    }

    /// Enrich both log output and span fields with a custom formatting function.
    ///
    /// This is shorthand for calling both `.enrich_log_custom::<T>(f)`
    /// and `.enrich_span_custom::<T>(f)` with the same function.
    pub fn enrich_custom<T: 'static>(
        self,
        f: impl Fn(&T) -> String + Send + Sync + 'static,
    ) -> Self {
        let fmt: Arc<dyn Fn(&dyn Any) -> Option<String> + Send + Sync> =
            Arc::new(move |any_val| any_val.downcast_ref::<T>().map(&f));
        Self {
            log_fmt_fn: Some(Arc::clone(&fmt)),
            span_fmt_fn: Some(fmt),
            ..self
        }
    }

    // ── Enrich log only (context → log event) ──────────────────

    /// Enrich log output only using [`Display`](std::fmt::Display).
    ///
    /// The value will appear in log events via [`WithContextFields`](crate::WithContextFields)
    /// but will NOT be automatically recorded into span fields.
    pub fn enrich_log_display<T: fmt::Display + 'static>(mut self) -> Self {
        self.log_fmt_fn = Some(Arc::new(|any_val| {
            any_val.downcast_ref::<T>().map(|v| v.to_string())
        }));
        self
    }

    /// Enrich log output only using [`Debug`](std::fmt::Debug).
    pub fn enrich_log_debug<T: fmt::Debug + 'static>(mut self) -> Self {
        self.log_fmt_fn = Some(Arc::new(|any_val| {
            any_val.downcast_ref::<T>().map(|v| format!("{:?}", v))
        }));
        self
    }

    /// Enrich log output only with a custom formatting function.
    pub fn enrich_log_custom<T: 'static>(
        mut self,
        f: impl Fn(&T) -> String + Send + Sync + 'static,
    ) -> Self {
        self.log_fmt_fn = Some(Arc::new(move |any_val| {
            any_val.downcast_ref::<T>().map(&f)
        }));
        self
    }

    // ── Record span only (context → span field) ────────────────

    /// Record into span fields using [`Display`](std::fmt::Display).
    ///
    /// On span enter, the current context value is formatted and recorded
    /// into the span field (must be pre-declared with `tracing::field::Empty`).
    /// Spans without the field are silently skipped.
    pub fn enrich_span_display<T: fmt::Display + 'static>(mut self) -> Self {
        self.span_fmt_fn = Some(Arc::new(|any_val| {
            any_val.downcast_ref::<T>().map(|v| v.to_string())
        }));
        self
    }

    /// Record into span fields using [`Debug`](std::fmt::Debug).
    pub fn enrich_span_debug<T: fmt::Debug + 'static>(mut self) -> Self {
        self.span_fmt_fn = Some(Arc::new(|any_val| {
            any_val.downcast_ref::<T>().map(|v| format!("{:?}", v))
        }));
        self
    }

    /// Record into span fields with a custom formatting function.
    ///
    /// On span enter, the current context value is passed to `f` and the
    /// result is recorded into the span field.
    pub fn enrich_span_custom<T: 'static>(
        mut self,
        f: impl Fn(&T) -> String + Send + Sync + 'static,
    ) -> Self {
        self.span_fmt_fn = Some(Arc::new(move |any_val| {
            any_val.downcast_ref::<T>().map(&f)
        }));
        self
    }

    /// Build the [`TracingField`] metadata.
    pub fn build(self) -> TracingField {
        let extract = if self.from_str.is_some()
            || self.from_u64.is_some()
            || self.from_i64.is_some()
            || self.from_bool.is_some()
        {
            Some(ExtractFns {
                from_str: self.from_str,
                from_u64: self.from_u64,
                from_i64: self.from_i64,
                from_bool: self.from_bool,
            })
        } else {
            None
        };

        TracingField {
            log_name: self.log_name,
            span_field: self.span_field,
            record_field: self.record_field,
            extract,
            log_fmt_fn: self.log_fmt_fn,
            span_fmt_fn: self.span_fmt_fn,
        }
    }
}

// ── Discovery cache ────────────────────────────────────────────

/// Cached info about a single TracingField entry, resolved from registry.
pub(crate) struct TracingFieldEntry {
    pub context_key: &'static str,
    /// Span field name for extraction (may differ from context_key).
    pub span_field: &'static str,
    /// Span field name to record into.
    pub record_field: &'static str,
    /// Extract closures (if extraction is enabled).
    pub extract: Option<ExtractFns>,
    /// Log field name (for log enrichment output).
    pub log_name: &'static str,
    /// Format function for log enrichment.
    pub log_fmt_fn: Option<Arc<dyn Fn(&dyn Any) -> Option<String> + Send + Sync>>,
    /// Format function for span recording.
    pub span_fmt_fn: Option<Arc<dyn Fn(&dyn Any) -> Option<String> + Send + Sync>>,
}

/// Global cache of discovered TracingField entries.
/// Populated lazily on first access (after registry is initialized).
static TRACING_FIELDS: OnceLock<Vec<TracingFieldEntry>> = OnceLock::new();

/// Get the cached list of TracingField entries, discovering from registry on first call.
pub(crate) fn get_tracing_fields() -> &'static [TracingFieldEntry] {
    TRACING_FIELDS.get_or_init(|| {
        dcontext::keys_with_metadata::<TracingField, _>(|key, meta| {
            let span_field = meta.span_field.unwrap_or(key);
            let record_field = meta.record_field.unwrap_or(meta.log_name);
            TracingFieldEntry {
                context_key: key,
                span_field,
                record_field,
                extract: meta.extract.as_ref().map(|e| ExtractFns {
                    from_str: e.from_str.as_ref().map(Arc::clone),
                    from_u64: e.from_u64.as_ref().map(Arc::clone),
                    from_i64: e.from_i64.as_ref().map(Arc::clone),
                    from_bool: e.from_bool.as_ref().map(Arc::clone),
                }),
                log_name: meta.log_name,
                log_fmt_fn: meta.log_fmt_fn.as_ref().map(Arc::clone),
                span_fmt_fn: meta.span_fmt_fn.as_ref().map(Arc::clone),
            }
        })
    })
}

// ── Collect log fields (public API) ────────────────────────────

/// Collect the current context log fields as `(name, formatted_value)` pairs.
///
/// Returns only fields that have log enrichment enabled and a value set in the
/// current context. Never panics.
///
/// This is the public primitive for custom formatters.
pub fn collect_log_fields() -> Vec<(&'static str, String)> {
    let entries = get_tracing_fields();
    let mut result = Vec::new();
    for entry in entries {
        if let Some(ref fmt_fn) = entry.log_fmt_fn {
            if let Some(formatted) = dcontext::with_context_value(entry.context_key, |any_val| {
                fmt_fn(any_val)
            })
            .flatten()
            {
                result.push((entry.log_name, formatted));
            }
        }
    }
    result
}

// ── FormatEvent wrapper ────────────────────────────────────────

use tracing::Subscriber;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// A [`FormatEvent`] wrapper that enriches log events with context values.
///
/// Wraps an inner formatter and prepends context fields to every log line.
/// Fields are discovered lazily from registry metadata on first use.
///
/// # Example
///
/// ```rust,ignore
/// use tracing_subscriber::prelude::*;
/// use dcontext_tracing::WithContextFields;
///
/// tracing_subscriber::registry()
///     .with(dcontext_tracing::DcontextLayer::new())
///     .with(tracing_subscriber::fmt::layer()
///         .event_format(WithContextFields::wrap(
///             tracing_subscriber::fmt::format().without_time()
///         )))
///     .init();
/// ```
pub struct WithContextFields<F> {
    inner: F,
}

impl<F> WithContextFields<F> {
    /// Wrap an inner formatter with context field enrichment.
    ///
    /// Fields are discovered lazily from the registry on first log event.
    /// Safe to call before `initialize()` — discovery happens on first use.
    pub fn wrap(inner: F) -> Self {
        Self { inner }
    }
}

impl<S, N, F> FormatEvent<S, N> for WithContextFields<F>
where
    F: FormatEvent<S, N>,
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        let entries = get_tracing_fields();
        for entry in entries {
            if let Some(ref fmt_fn) = entry.log_fmt_fn {
                if let Some(formatted) =
                    dcontext::with_context_value(entry.context_key, |any_val| fmt_fn(any_val))
                        .flatten()
                {
                    write!(writer, "{}={} ", entry.log_name, formatted)?;
                }
            }
        }
        self.inner.format_event(ctx, writer, event)
    }
}
