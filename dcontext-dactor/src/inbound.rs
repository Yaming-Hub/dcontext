use std::any::Any;

use dactor::{
    Disposition, HandlerWrapper, Headers, InboundContext, InboundInterceptor, Outcome,
    RuntimeHeaders,
};

use crate::header::{ContextHeader, ContextSnapshotHeader};
use crate::propagation::bytes_to_snapshot;
use crate::ErrorPolicy;

/// Inbound interceptor that propagates dcontext automatically through actor
/// message handlers.
///
/// This interceptor performs two complementary actions:
///
/// 1. **`on_receive`** — Normalizes context headers: if only wire bytes
///    ([`ContextHeader`]) are present (remote hop), deserializes them into a
///    [`ContextSnapshotHeader`]. Local snapshots are left as-is.
///
/// 2. **`wrap_handler`** — Wraps the handler future with
///    [`dcontext::with_context`], restoring the propagated context into the
///    async task-local scope. This makes `dcontext::get_context` /
///    `dcontext::set_context` work automatically inside the handler — **no
///    manual `with_propagated_context()` call needed**.
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
/// use dcontext_dactor::{ContextOutboundInterceptor, ContextInboundInterceptor};
///
/// // Register interceptors — context propagation is fully automatic
/// runtime.add_outbound_interceptor(Box::new(ContextOutboundInterceptor::default()));
/// runtime.add_inbound_interceptor(Box::new(ContextInboundInterceptor::default()));
///
/// // In the handler, dcontext is available without any boilerplate:
/// #[async_trait]
/// impl Handler<MyMessage> for MyActor {
///     async fn handle(&mut self, msg: MyMessage, ctx: &mut ActorContext) -> () {
///         let rid: RequestId = dcontext::get_context("request_id");
///         // ... context is automatically restored by the interceptor
///     }
/// }
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

    fn wrap_handler<'a>(
        &'a self,
        ctx: &InboundContext<'_>,
        headers: &Headers,
    ) -> Option<HandlerWrapper<'a>> {
        // Extract the snapshot prepared by on_receive (or outbound for local hops).
        let snapshot = headers.get::<ContextSnapshotHeader>()?.snapshot.clone();
        // Create a named scope for the remote call boundary, e.g. "remote:MyActor".
        let scope_name = format!("remote:{}", ctx.actor_name);
        Some(Box::new(move |next| {
            Box::pin(async move {
                let result = dcontext::with_context(snapshot, async move {
                    dcontext::named_scope_async(scope_name, next).await
                }).await;
                result
            })
        }))
    }

    fn on_complete(
        &self,
        _ctx: &InboundContext<'_>,
        _runtime_headers: &RuntimeHeaders,
        _headers: &Headers,
        _outcome: &Outcome<'_>,
    ) {
        // No cleanup needed — context scope ends when the wrapped future completes.
    }
}
