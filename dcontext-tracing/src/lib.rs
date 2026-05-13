//! # dcontext-tracing
//!
//! Bidirectional bridge between [dcontext](https://docs.rs/dcontext) and
//! [tracing](https://docs.rs/tracing) spans.
//!
//! This crate provides [`tracing_subscriber::Layer`] implementations that
//! copy data between tracing span fields and dcontext context values.
//! It does **not** manage dcontext scopes — scope lifecycle remains the
//! caller's responsibility.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use tracing_subscriber::prelude::*;
//!
//! // Install the layer — field extraction and recording happen automatically
//! tracing_subscriber::registry()
//!     .with(dcontext_tracing::SyncDcontextLayer::new())
//!     .init();
//! ```
//!
//! ## Features
//!
//! ### Field Extraction (span → context)
//!
//! When a tracing span contains a field that matches a registered
//! [`TracingField`], the value is extracted and set as a context variable.
//!
//! ```rust,no_run
//! # use tracing_subscriber::prelude::*;
//! # tracing_subscriber::registry()
//! #     .with(dcontext_tracing::SyncDcontextLayer::new())
//! #     .init();
//! #
//! // With TracingField metadata registered for "request_id":
//! // let _span = tracing::info_span!("handler", request_id = "abc-123").entered();
//! // → dcontext now has request_id = "abc-123"
//! ```
//!
//! ### Log & Span Enrichment (context → log/span)
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
//! ### Span Info
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
//! // let info: SpanInfo = dcontext::sync_ctx::get_context("dcontext.span").unwrap_or_default();
//! // info.name, info.target, info.level
//! ```
//!
//! ## How It Works
//!
//! The layer hooks into tracing's span lifecycle (`on_new_span`, `on_enter`,
//! `on_close`) to copy data between spans and the dcontext store. It does
//! **not** push or pop dcontext scopes — that responsibility stays with
//! application code, which can manage scopes independently of span lifetime.
//!
//! `SyncDcontextLayer` reads/writes `dcontext::sync_ctx` (thread-local).
//! `AsyncDcontextLayer` reads/writes `dcontext::async_ctx` (task-local) and
//! performs extraction only on the first enter of each span.
//!
//! ## Async Behavior
//!
//! Tokio async code should use [`AsyncDcontextLayer`], which performs field
//! extraction into `dcontext::async_ctx` task-local storage on first span
//! enter. Values persist across `.await` points in the task.
//!
//! [`SyncDcontextLayer`] (and the legacy [`DcontextLayer`] alias) remain useful
//! for synchronous or explicitly thread-local code.

mod async_layer;
mod field_mapping;
mod layer_common;
mod span_info;
mod sync_layer;
mod tracing_field;

#[cfg(test)]
mod tests;

pub use async_layer::{AsyncDcontextLayer, AsyncDcontextLayerBuilder};
pub use sync_layer::{SyncDcontextLayer, SyncDcontextLayerBuilder};
/// Type alias for backward compatibility — `DcontextLayer` is now `SyncDcontextLayer`.
pub type DcontextLayer<S> = SyncDcontextLayer<S>;
/// Type alias for backward compatibility — `DcontextLayerBuilder` is now `SyncDcontextLayerBuilder`.
pub type DcontextLayerBuilder<S> = SyncDcontextLayerBuilder<S>;
pub use span_info::{SpanInfo, SPAN_INFO_KEY};
pub use tracing_field::{collect_log_fields, TracingField, TracingFieldBuilder, WithContextFields};
