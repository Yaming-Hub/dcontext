//! # dcontext-tracing
//!
//! Automatic [dcontext](https://docs.rs/dcontext) scope management via
//! [tracing](https://docs.rs/tracing) spans.
//!
//! This crate provides a [`tracing_subscriber::Layer`] that automatically
//! creates and manages dcontext scopes when tracing spans are entered and
//! exited. This means your context values follow the natural span lifecycle
//! without any manual scope management.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use tracing_subscriber::prelude::*;
//!
//! // Zero-config: every span creates a dcontext scope
//! tracing_subscriber::registry()
//!     .with(dcontext_tracing::DcontextLayer::new())
//!     .init();
//! ```
//!
//! ## Features
//!
//! ### Level 1: Automatic Scoping
//!
//! With zero configuration, `DcontextLayer` creates a new dcontext scope
//! every time a span is entered. Values set inside a span are automatically
//! cleaned up when the span exits, just like tracing's own span lifecycle.
//!
//! ```rust,no_run
//! # use tracing_subscriber::prelude::*;
//! # tracing_subscriber::registry()
//! #     .with(dcontext_tracing::DcontextLayer::new())
//! #     .init();
//! #
//! // Register context keys, then inside a span:
//! // dcontext::set_context("user", "alice".to_string());
//! // {
//! //     let _span = tracing::info_span!("request").entered();
//! //     // New scope created — inherits parent values
//! //     dcontext::set_context("request_id", "abc-123".to_string());
//! // }
//! // Scope reverted — "request_id" gone, "user" remains
//! ```
//!
//! ### Level 2: Field-to-Context Extraction
//!
//! Extract tracing span fields directly into dcontext values using
//! [`TracingField`] metadata:
//!
//! ```rust,no_run
//! use dcontext_tracing::{DcontextLayer, TracingField};
//!
//! #[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
//! struct RequestId(String);
//!
//! impl std::fmt::Display for RequestId {
//!     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//!         write!(f, "{}", self.0)
//!     }
//! }
//!
//! let mut builder = dcontext::RegistryBuilder::new();
//! builder.register_with::<RequestId>("request_id", |opts| {
//!     opts.with_metadata(
//!         TracingField::builder("request_id")
//!             .extract_from_str(|s| Some(RequestId(s.to_string())))
//!             .enrich_display::<RequestId>()  // enables both log + span enrichment
//!             .build(),
//!     )
//! });
//!
//! // DcontextLayer discovers TracingField metadata automatically.
//! // On span enter:
//! //   - Extracts span fields into context (extract direction)
//! //   - Records context values into pre-declared Empty span fields (span record direction)
//! // let layer = DcontextLayer::new();
//! ```
//!
//! ### Level 3: Span Info
//!
//! Expose span metadata as a context value:
//!
//! ```rust,no_run
//! use dcontext_tracing::{DcontextLayer, SpanInfo};
//! use tracing_subscriber::Registry;
//!
//! let layer: DcontextLayer<Registry> = DcontextLayer::builder()
//!     .include_span_info()
//!     .build();
//!
//! // Inside a span:
//! // let info: SpanInfo = dcontext::get_context("dcontext.span");
//! // info.name, info.target, info.level
//! ```
//!
//! ## How It Works
//!
//! The layer uses a thread-local stack to store dcontext `ScopeGuard`s
//! (which are `!Send` and cannot be stored in tracing's span extensions).
//! On span enter, a new scope is pushed; on span exit, the scope is popped
//! and the guard dropped, reverting context changes made in that scope.
//!
//! This mirrors the approach used by `tracing-opentelemetry` for similar
//! thread-local guard management.
//!
//! ## Async Behavior
//!
//! When used with [`Instrument`](tracing::Instrument), the layer creates and
//! reverts a scope around each poll of the future. Mapped field values and span
//! info are re-applied on each enter, so reads via `force_thread_local()` will
//! see the correct values during each poll. However, **mutations made inside a
//! span do not persist across `.await` points** — each poll gets a fresh scope.
//!
//! For full async context propagation across `.await`, use `dcontext::with_context()`
//! or `dcontext::ContextFuture` directly.

mod field_mapping;
pub(crate) mod guard_stack;
mod span_info;
mod tracing_field;
mod async_layer;
mod sync_layer;

#[cfg(test)]
mod tests;

pub use async_layer::{AsyncDcontextLayer, AsyncDcontextLayerBuilder};
pub use sync_layer::{SyncDcontextLayer, SyncDcontextLayerBuilder};
/// Type alias for backward compatibility — `DcontextLayer` is now `SyncDcontextLayer`.
pub type DcontextLayer<S> = SyncDcontextLayer<S>;
/// Type alias for backward compatibility — `DcontextLayerBuilder` is now `SyncDcontextLayerBuilder`.
pub type DcontextLayerBuilder<S> = SyncDcontextLayerBuilder<S>;
pub use tracing_field::{TracingField, TracingFieldBuilder, WithContextFields, collect_log_fields};
pub use span_info::{SpanInfo, SPAN_INFO_KEY};
