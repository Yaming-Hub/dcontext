//! Log enrichment: automatically inject context values into structured log output.
//!
//! Register [`LogField`] metadata on context keys to have them automatically
//! included in every log event when using [`WithContextFields`] as the event
//! formatter.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

// ── LogField metadata ──────────────────────────────────────────

/// Metadata that marks a context key for automatic inclusion in log output.
///
/// Attach this to a registration via [`RegistrationOptions::with_metadata`]:
///
/// ```rust,ignore
/// use dcontext_tracing::LogField;
///
/// builder.register_with::<RequestId>("request_id", |opts| {
///     opts.cached().with_metadata(LogField::display::<RequestId>("rid"))
/// });
/// ```
///
/// The formatter function is captured at registration time — the concrete
/// type is known then, so downcasting + formatting is type-safe.
pub struct LogField {
    /// Field name to use in log output.
    name: &'static str,
    /// Type-erased formatter: downcasts `&dyn Any` to the concrete type and
    /// formats it as a string. Returns `None` if the downcast fails.
    fmt_fn: Arc<dyn Fn(&dyn Any) -> Option<String> + Send + Sync>,
}

impl LogField {
    /// Create a `LogField` that formats values using [`Display`](std::fmt::Display).
    ///
    /// Best for simple types like strings, IDs, and numbers.
    pub fn display<T: fmt::Display + 'static>(name: &'static str) -> Self {
        Self {
            name,
            fmt_fn: Arc::new(|any_val| {
                any_val.downcast_ref::<T>().map(|v| v.to_string())
            }),
        }
    }

    /// Create a `LogField` that formats values using [`Debug`](std::fmt::Debug).
    ///
    /// Useful for types that implement Debug but not Display.
    pub fn debug<T: fmt::Debug + 'static>(name: &'static str) -> Self {
        Self {
            name,
            fmt_fn: Arc::new(|any_val| {
                any_val.downcast_ref::<T>().map(|v| format!("{:?}", v))
            }),
        }
    }

    /// Create a `LogField` with a custom formatting function.
    ///
    /// The function receives `&T` and returns the formatted string.
    pub fn custom<T: 'static>(
        name: &'static str,
        f: impl Fn(&T) -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            fmt_fn: Arc::new(move |any_val| {
                any_val.downcast_ref::<T>().map(&f)
            }),
        }
    }

    /// The field name used in log output.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Format a type-erased value. Returns `None` if the value cannot be
    /// downcast to the expected type.
    pub fn format(&self, any_val: &dyn Any) -> Option<String> {
        (self.fmt_fn)(any_val)
    }
}

// ── Collect log fields ─────────────────────────────────────────

/// Info about a single log-enrichment field, cached at formatter construction.
pub(crate) struct LogFieldEntry {
    pub context_key: &'static str,
    pub log_field: LogField,
}

/// Discover all registered keys with [`LogField`] metadata.
///
/// Called once at formatter construction time to cache the field list.
pub(crate) fn discover_log_fields() -> Vec<LogFieldEntry> {
    dcontext::keys_with_metadata::<LogField, _>(|key, meta| LogFieldEntry {
        context_key: key,
        log_field: LogField {
            name: meta.name,
            fmt_fn: Arc::clone(&meta.fmt_fn),
        },
    })
}

/// Collect the current context log fields as `(name, formatted_value)` pairs.
///
/// This is the public primitive for custom formatters. Returns only fields
/// that have a value set in the current context. Never panics.
pub fn collect_log_fields() -> Vec<(&'static str, String)> {
    let entries = discover_log_fields();
    let mut result = Vec::new();
    for entry in &entries {
        if let Some(formatted) = dcontext::with_context_value(entry.context_key, |any_val| {
            entry.log_field.format(any_val)
        }).flatten() {
            result.push((entry.log_field.name, formatted));
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
/// Wraps an inner formatter and appends context fields to every log line.
/// Fields are discovered from registry metadata at construction time.
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
///             tracing_subscriber::fmt::format::Full::default()
///         )))
///     .init();
/// ```
pub struct WithContextFields<F> {
    inner: F,
    fields: Vec<LogFieldEntry>,
}

impl<F> WithContextFields<F> {
    /// Wrap an inner formatter with context field enrichment.
    ///
    /// Log fields are discovered from the registry at this point.
    /// Call this after all registrations are complete (after `initialize()`).
    pub fn wrap(inner: F) -> Self {
        Self {
            inner,
            fields: discover_log_fields(),
        }
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
        // Collect context fields. If any fail to format, skip silently.
        let mut context_parts: Vec<(&str, String)> = Vec::new();
        for entry in &self.fields {
            if let Some(formatted) = dcontext::with_context_value(entry.context_key, |any_val| {
                entry.log_field.format(any_val)
            }).flatten() {
                context_parts.push((entry.log_field.name, formatted));
            }
        }

        // Write context fields as prefix: `key=value key=value `
        for (name, value) in &context_parts {
            write!(writer, "{}={} ", name, value)?;
        }

        // Delegate to inner formatter for the actual event
        self.inner.format_event(ctx, writer, event)
    }
}
