use std::any::Any;

use dactor::{
    Disposition, Headers, OutboundContext, OutboundInterceptor, Outcome, RuntimeHeaders,
};

use crate::header::{ContextHeader, ContextSnapshotHeader};

/// Outbound interceptor that captures the current dcontext and attaches it
/// to outgoing actor messages as headers.
///
/// - **Local targets**: attaches a [`ContextSnapshotHeader`] (preserves
///   local-only context values) **and** a [`ContextHeader`] (wire bytes).
/// - **Remote targets**: attaches only [`ContextHeader`] (wire bytes).
///
/// # Usage
///
/// Register on your actor runtime:
///
/// ```ignore
/// use dcontext_dactor::ContextOutboundInterceptor;
///
/// runtime.add_outbound_interceptor(Box::new(ContextOutboundInterceptor));
/// ```
pub struct ContextOutboundInterceptor;

impl OutboundInterceptor for ContextOutboundInterceptor {
    fn name(&self) -> &'static str {
        "dcontext-outbound"
    }

    fn on_send(
        &self,
        ctx: &OutboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        headers: &mut Headers,
        _message: &dyn Any,
    ) -> Disposition {
        // For local targets, capture a full snapshot (preserves local-only values).
        if !ctx.remote {
            let snap = dcontext::snapshot();
            headers.insert(ContextSnapshotHeader { snapshot: snap });
        }

        // Always serialize wire bytes (needed for remote, useful as fallback for local).
        match dcontext::serialize_context() {
            Ok(bytes) => {
                headers.insert(ContextHeader { bytes });
            }
            Err(e) => {
                tracing::warn!(
                    target: "dcontext_dactor",
                    error = %e,
                    target_actor = %ctx.target_name,
                    "failed to serialize dcontext for outbound message"
                );
            }
        }

        Disposition::Continue
    }

    fn on_reply(
        &self,
        _ctx: &OutboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &Headers,
        _outcome: &Outcome<'_>,
    ) {
        // No action on reply — context flows forward, not backward.
    }
}
