//! # dcontext-dactor
//!
//! Automatic dcontext propagation through [dactor](https://github.com/Yaming-Hub/dactor)
//! actor messages.
//!
//! This crate bridges [`dcontext`] (distributed context propagation) with
//! [`dactor`] (actor framework) by providing interceptors and helpers that
//! automatically carry context across actor message boundaries.
//!
//! ## How It Works
//!
//! Context propagation is a **three-stage pipeline**:
//!
//! 1. **Outbound interceptor** (sender side) — [`ContextOutboundInterceptor`]
//!    captures the current dcontext and attaches it as message headers.
//!    - Local targets → [`ContextSnapshotHeader`] (preserves local-only values)
//!    - Remote targets → [`ContextHeader`] (serialized wire bytes)
//!
//! 2. **Inbound interceptor** (receiver side) — [`ContextInboundInterceptor`]
//!    normalizes the headers: if only wire bytes are present (remote hop), it
//!    deserializes them into a [`ContextSnapshotHeader`]. **This does NOT
//!    restore context into the async task** — it only prepares the snapshot
//!    in headers for the handler.
//!
//! 3. **Handler helper** (inside the handler) — The handler calls
//!    [`with_propagated_context`] to establish a dcontext task-local scope
//!    for its async body. This is the step that actually makes `get_context` /
//!    `set_context` work inside the handler.
//!
//! The reason the inbound interceptor cannot directly restore context is that
//! dcontext's [`ScopeGuard`](dcontext::ScopeGuard) is `!Send`, so it cannot
//! be held across the async handler boundary. Instead,
//! [`with_propagated_context`] uses [`dcontext::with_context`] to wrap the
//! handler future in a properly scoped task-local.
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
//! use dcontext_dactor::{
//!     ContextOutboundInterceptor,
//!     ContextInboundInterceptor,
//!     with_propagated_context,
//! };
//!
//! // 1. Register interceptors on your runtime
//! runtime.add_outbound_interceptor(Box::new(ContextOutboundInterceptor::default()));
//! runtime.add_inbound_interceptor(Box::new(ContextInboundInterceptor::default()));
//!
//! // 2. In your handler, wrap the body with propagated context
//! #[async_trait]
//! impl Handler<MyMessage> for MyActor {
//!     async fn handle(&mut self, msg: MyMessage, ctx: &mut ActorContext) -> () {
//!         with_propagated_context(ctx, async {
//!             // dcontext task-local scope is active here
//!             let rid: RequestId = dcontext::get_context("request_id");
//!             // ... handle message with context available ...
//!         }).await;
//!     }
//! }
//! ```

mod header;
mod inbound;
mod outbound;
mod propagation;

pub use header::{ContextHeader, ContextSnapshotHeader};
pub use inbound::ContextInboundInterceptor;
pub use outbound::ContextOutboundInterceptor;
pub use propagation::{extract_context, with_propagated_context};

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
