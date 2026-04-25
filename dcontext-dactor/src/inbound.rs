use std::any::Any;

use dactor::{
    Disposition, Headers, InboundContext, InboundInterceptor, Outcome, RuntimeHeaders,
};

use crate::header::{ContextHeader, ContextSnapshotHeader};
use crate::propagation::bytes_to_snapshot;

/// Inbound interceptor that extracts propagated dcontext from message headers
/// and converts it into a [`ContextSnapshotHeader`] for handler consumption.
///
/// On receive:
/// - If a [`ContextSnapshotHeader`] already exists (local hop), it is left as-is.
/// - If only [`ContextHeader`] (wire bytes) exists, deserializes it into a
///   [`ContextSnapshotHeader`] so the handler can restore context uniformly.
///
/// The handler then calls [`with_propagated_context`](crate::with_propagated_context)
/// to establish the dcontext scope for its async execution.
///
/// # Usage
///
/// ```ignore
/// use dcontext_dactor::ContextInboundInterceptor;
///
/// runtime.add_inbound_interceptor(Box::new(ContextInboundInterceptor));
/// ```
pub struct ContextInboundInterceptor;

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
        // No cleanup needed — context scope is managed by the handler's guard.
    }
}
