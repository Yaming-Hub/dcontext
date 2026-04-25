use std::any::Any;

use dactor::{
    Disposition, Headers, OutboundContext, OutboundInterceptor, Outcome, RuntimeHeaders,
};

use crate::header::{ContextHeader, ContextSnapshotHeader};
use crate::ErrorPolicy;

/// Outbound interceptor that captures the current dcontext and attaches it
/// to outgoing actor messages as headers.
///
/// - **Local targets**: attaches a [`ContextSnapshotHeader`] only — no
///   serialization is performed, preserving local-only context values.
/// - **Remote targets**: serializes context to bytes and attaches a
///   [`ContextHeader`] for wire transport.
///
/// # Error Handling
///
/// Controlled by [`ErrorPolicy`]:
/// - [`LogAndContinue`](ErrorPolicy::LogAndContinue) (default) — log and
///   send the message without context.
/// - [`Reject`](ErrorPolicy::Reject) — reject the message.
///
/// # Usage
///
/// ```ignore
/// use dcontext_dactor::{ContextOutboundInterceptor, ErrorPolicy};
///
/// // Default: log and continue on errors
/// runtime.add_outbound_interceptor(Box::new(ContextOutboundInterceptor::default()));
///
/// // Strict: reject messages if context serialization fails
/// runtime.add_outbound_interceptor(Box::new(
///     ContextOutboundInterceptor::new(ErrorPolicy::Reject),
/// ));
/// ```
pub struct ContextOutboundInterceptor {
    error_policy: ErrorPolicy,
}

impl ContextOutboundInterceptor {
    /// Create with a specific error policy.
    pub fn new(error_policy: ErrorPolicy) -> Self {
        Self { error_policy }
    }
}

impl Default for ContextOutboundInterceptor {
    fn default() -> Self {
        Self {
            error_policy: ErrorPolicy::default(),
        }
    }
}

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
        if ctx.remote {
            // Remote target: serialize context to wire bytes.
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
                    if self.error_policy == ErrorPolicy::Reject {
                        return Disposition::Reject(format!(
                            "dcontext serialization failed: {e}"
                        ));
                    }
                }
            }
        } else {
            // Local target: snapshot only — no serialization needed.
            let snap = dcontext::snapshot();
            headers.insert(ContextSnapshotHeader { snapshot: snap });
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
