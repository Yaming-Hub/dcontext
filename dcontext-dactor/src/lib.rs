//! # dcontext-dactor
//!
//! Automatic dcontext propagation through [dactor](https://github.com/Yaming-Hub/dactor)
//! actor messages.
//!
//! This crate bridges [`dcontext`] (distributed context propagation) with
//! [`dactor`] (actor framework) by providing interceptors that automatically
//! carry context across actor message boundaries — **no boilerplate needed
//! in handlers**.
//!
//! ## How It Works
//!
//! Context propagation is a **two-stage interceptor pipeline**:
//!
//! 1. **Outbound interceptor** (sender side) — [`ContextOutboundInterceptor`]
//!    captures the current dcontext and attaches it as message headers.
//!    - Local targets → [`ContextSnapshotHeader`] (preserves local-only values)
//!    - Remote targets → [`ContextHeader`] (serialized wire bytes)
//!
//! 2. **Inbound interceptor** (receiver side) — [`ContextInboundInterceptor`]
//!    performs two actions:
//!    - `on_receive`: normalizes headers — deserializes wire bytes into a
//!      [`ContextSnapshotHeader`] if needed.
//!    - `wrap_handler`: wraps the handler future with
//!      [`dcontext::with_context`], restoring the propagated snapshot into
//!      the async task-local scope automatically.
//!
//! This uses dactor 0.3's `wrap_handler` feature to wrap the handler future
//! with a task-local context scope. `dcontext::get_context` /
//! `dcontext::set_context` work transparently inside the handler.
//!
//! ## Error Handling
//!
//! Both interceptors accept an [`ErrorPolicy`] that controls behavior when
//! serialization or deserialization fails:
//!
//! - [`ErrorPolicy::LogAndContinue`] (default) — log a warning, deliver the
//!   message without context.
//! - [`ErrorPolicy::Reject`] — reject the message via
//!   [`Disposition::Reject`](dactor::Disposition::Reject).
//!
//! ## Local vs. Remote
//!
//! - **Local** (same process): Context is propagated via
//!   [`ContextSnapshot`](dcontext::ContextSnapshot), preserving local-only
//!   values that cannot be serialized. **No serialization is performed.**
//!
//! - **Remote** (cross-process): Context is serialized to bytes via
//!   [`dcontext::serialize_context`] and transmitted as a wire header.
//!   Local-only values are excluded.
//!
//! ## Quick Start
//!
//! ```ignore
//! use dcontext_dactor::{ContextOutboundInterceptor, ContextInboundInterceptor};
//!
//! // Register interceptors on your runtime — that's it!
//! runtime.add_outbound_interceptor(Box::new(ContextOutboundInterceptor::default()));
//! runtime.add_inbound_interceptor(Box::new(ContextInboundInterceptor::default()));
//!
//! // Handlers automatically have dcontext available — no boilerplate
//! #[async_trait]
//! impl Handler<MyMessage> for MyActor {
//!     async fn handle(&mut self, msg: MyMessage, ctx: &mut ActorContext) -> () {
//!         // dcontext is automatically restored by the interceptor's wrap_handler
//!         let rid: RequestId = dcontext::get_context("request_id");
//!         // ... handle message with context available ...
//!     }
//! }
//! ```
//!
//! ## Manual Extraction
//!
//! For advanced use cases (e.g., spawning sub-tasks that need context),
//! [`extract_context`] is still available to pull the snapshot from headers.
//! [`with_propagated_context`] is retained for backward compatibility but is
//! no longer needed when using the interceptor pipeline.

mod header;
mod inbound;
mod outbound;
mod propagation;

pub use header::{ContextHeader, ContextSnapshotHeader};
pub use inbound::ContextInboundInterceptor;
pub use outbound::ContextOutboundInterceptor;
pub use propagation::extract_context;

#[allow(deprecated)]
pub use propagation::with_propagated_context;

/// Controls how interceptors behave when serialization or deserialization fails.
///
/// Passed to [`ContextOutboundInterceptor::new`] and
/// [`ContextInboundInterceptor::new`] to configure error handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorPolicy {
    /// Log a warning and deliver the message without propagated context.
    /// This is the default.
    LogAndContinue,
    /// Reject the message via [`Disposition::Reject`](dactor::Disposition::Reject).
    Reject,
}

impl Default for ErrorPolicy {
    fn default() -> Self {
        Self::LogAndContinue
    }
}

#[cfg(test)]
mod tests;
