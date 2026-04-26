use dactor::ActorContext;
use dcontext::ContextSnapshot;

use crate::header::{ContextHeader, ContextSnapshotHeader};

/// Extract the propagated context snapshot from actor message headers.
///
/// Checks for a [`ContextSnapshotHeader`] (local hop) first, then falls
/// back to [`ContextHeader`] (remote wire bytes). Returns `None` if no
/// propagated context is present.
///
/// Useful for advanced scenarios such as spawning sub-tasks that need the
/// propagated context, or inspecting context values without relying on
/// the task-local scope.
pub fn extract_context(ctx: &ActorContext) -> Option<ContextSnapshot> {
    // Prefer local snapshot (preserves local-only values).
    if let Some(h) = ctx.headers.get::<ContextSnapshotHeader>() {
        return Some(h.snapshot.clone());
    }

    // Fall back to wire bytes.
    if let Some(h) = ctx.headers.get::<ContextHeader>() {
        return bytes_to_snapshot(&h.bytes);
    }

    None
}

/// Run an async handler body with the propagated dcontext from actor headers.
///
/// **Deprecated since dactor 0.3**: The [`ContextInboundInterceptor`](crate::ContextInboundInterceptor)
/// now implements `wrap_handler` which automatically restores context into the
/// handler's async task-local scope. This function is no longer needed when
/// using the interceptor pipeline.
///
/// Retained for backward compatibility and for use cases outside the
/// interceptor pipeline (e.g., manual context restoration in tests or
/// one-off async blocks).
///
/// If no propagated context is found in the headers, the future runs without
/// any dcontext scope (a no-op passthrough).
#[deprecated(
    since = "0.2.0",
    note = "ContextInboundInterceptor now restores context automatically via wrap_handler. \
            Use this only for manual context restoration outside the interceptor pipeline."
)]
pub async fn with_propagated_context<F, R>(ctx: &ActorContext, f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    match extract_context(ctx) {
        Some(snap) => dcontext::with_context(snap, f).await,
        None => f.await,
    }
}

/// Convert serialized wire bytes into a `ContextSnapshot`.
///
/// Uses `force_thread_local` to temporarily deserialize into thread-local
/// storage, captures a snapshot, then reverts. This avoids interfering
/// with any active task-local context.
pub(crate) fn bytes_to_snapshot(bytes: &[u8]) -> Option<ContextSnapshot> {
    dcontext::force_thread_local(|| {
        // Push a temporary scope, deserialize wire values into it,
        // capture as snapshot, then let guards revert everything.
        let _outer = dcontext::enter_scope();
        let _wire_guard = dcontext::deserialize_context(bytes).ok()?;
        Some(dcontext::snapshot())
        // _wire_guard drops → pops deserialized scope
        // _outer drops → pops isolation scope
    })
}
