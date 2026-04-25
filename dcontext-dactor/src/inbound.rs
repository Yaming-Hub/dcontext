use std::any::Any;

use dactor::{
    Disposition, Headers, InboundContext, InboundInterceptor, Outcome, RuntimeHeaders,
};

use crate::header::{ContextHeader, ContextSnapshotHeader};
use crate::propagation::bytes_to_snapshot;
use crate::ErrorPolicy;

/// Inbound interceptor that normalizes propagated dcontext in message headers.
///
/// This interceptor **does not restore context into the async task** — it only
/// prepares a [`ContextSnapshotHeader`] in the message headers so the handler
/// can uniformly consume it via [`with_propagated_context`](crate::with_propagated_context).
///
/// The reason context cannot be restored here is that dcontext's
/// [`ScopeGuard`](dcontext::ScopeGuard) is `!Send` and cannot be held across
/// the async handler boundary. The handler must call
/// [`with_propagated_context`](crate::with_propagated_context) to establish
/// a properly scoped task-local via [`dcontext::with_context`].
///
/// ## Behavior
///
/// - If a [`ContextSnapshotHeader`] already exists (local hop), it is left as-is.
/// - If only [`ContextHeader`] (wire bytes) exists (remote hop), deserializes
///   it into a [`ContextSnapshotHeader`].
///
/// ## Error Handling
///
/// Controlled by [`ErrorPolicy`]:
/// - [`LogAndContinue`](ErrorPolicy::LogAndContinue) (default) — log and
///   deliver the message without context.
/// - [`Reject`](ErrorPolicy::Reject) — reject the message.
///
/// # Usage
///
/// ```ignore
/// use dcontext_dactor::{ContextInboundInterceptor, ErrorPolicy};
///
/// // Default: log and continue on deserialization errors
/// runtime.add_inbound_interceptor(Box::new(ContextInboundInterceptor::default()));
///
/// // Strict: reject messages with corrupt context bytes
/// runtime.add_inbound_interceptor(Box::new(
///     ContextInboundInterceptor::new(ErrorPolicy::Reject),
/// ));
/// ```
pub struct ContextInboundInterceptor {
    error_policy: ErrorPolicy,
}

impl ContextInboundInterceptor {
    /// Create with a specific error policy.
    pub fn new(error_policy: ErrorPolicy) -> Self {
        Self { error_policy }
    }
}

impl Default for ContextInboundInterceptor {
    fn default() -> Self {
        Self {
            error_policy: ErrorPolicy::default(),
        }
    }
}

impl InboundInterceptor for ContextInboundInterceptor {
    fn name(&self) -> &'static str {
        "dcontext-inbound"
    }

    fn on_receive(
        &self,
        _ctx: &InboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        headers: &mut Headers,
        _message: &dyn Any,
    ) -> Disposition {
        // If a local snapshot header already exists, nothing to do.
        if headers.get::<ContextSnapshotHeader>().is_some() {
            return Disposition::Continue;
        }

        // Try to convert wire bytes into a snapshot for uniform handler access.
        if let Some(wire_header) = headers.get::<ContextHeader>() {
            let bytes = &wire_header.bytes;
            match bytes_to_snapshot(bytes) {
                Some(snap) => {
                    headers.insert(ContextSnapshotHeader { snapshot: snap });
                }
                None => {
                    tracing::warn!(
                        target: "dcontext_dactor",
                        "failed to deserialize dcontext from inbound wire bytes"
                    );
                    if self.error_policy == ErrorPolicy::Reject {
                        return Disposition::Reject(
                            "dcontext deserialization failed: corrupt wire bytes".into(),
                        );
                    }
                }
            }
        }

        Disposition::Continue
    }

    fn on_complete(
        &self,
        _ctx: &InboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &Headers,
        _outcome: &Outcome<'_>,
    ) {
        // No cleanup needed — context scope is managed by the handler via
        // with_propagated_context().
    }
}
