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
//! 1. **Outbound interceptor** — When an actor sends a message,
//!    [`ContextOutboundInterceptor`] captures the current dcontext and
//!    attaches it as message headers.
//!
//! 2. **Inbound interceptor** — When a message arrives,
//!    [`ContextInboundInterceptor`] extracts the propagated context and
//!    prepares it for handler consumption.
//!
//! 3. **Handler helper** — Inside the handler, call
//!    [`with_propagated_context`] to establish the dcontext scope for the
//!    async handler body.
//!
//! ## Local vs. Remote
//!
//! - **Local** (same process): Context is propagated via
//!   [`ContextSnapshot`](dcontext::ContextSnapshot), preserving local-only
//!   values that cannot be serialized.
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
//! runtime.add_outbound_interceptor(Box::new(ContextOutboundInterceptor));
//! runtime.add_inbound_interceptor(Box::new(ContextInboundInterceptor));
//!
//! // 2. In your handler, wrap the body with propagated context
//! #[async_trait]
//! impl Handler<MyMessage> for MyActor {
//!     async fn handle(&mut self, msg: MyMessage, ctx: &mut ActorContext) -> () {
//!         with_propagated_context(ctx, async {
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

#[cfg(test)]
mod tests;
