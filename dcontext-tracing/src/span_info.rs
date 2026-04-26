/// Span metadata exposed as a dcontext value.
///
/// When [`DcontextLayer`](crate::DcontextLayer) is configured with
/// [`include_span_info()`](crate::DcontextLayerBuilder::include_span_info),
/// this type is automatically set in the context on span enter.
///
/// # Example
///
/// ```ignore
/// use dcontext_tracing::SpanInfo;
///
/// let info: SpanInfo = dcontext::get_context("dcontext.span");
/// println!("current span: {} ({})", info.name, info.target);
/// ```
#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
pub struct SpanInfo {
    /// The span's name (from `#[instrument]` or `tracing::span!`).
    pub name: String,
    /// The span's target (usually the module path).
    pub target: String,
    /// The span's level as a string ("TRACE", "DEBUG", "INFO", "WARN", "ERROR").
    pub level: String,
}

/// The dcontext key used for [`SpanInfo`].
pub const SPAN_INFO_KEY: &str = "dcontext.span";
