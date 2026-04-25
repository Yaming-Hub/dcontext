use dactor::ActorContext;
use dcontext::ContextSnapshot;

use crate::header::{ContextHeader, ContextSnapshotHeader};

/// Extract the propagated context snapshot from actor message headers.
///
/// Checks for a [`ContextSnapshotHeader`] (local hop) first, then falls
/// back to [`ContextHeader`] (remote wire bytes). Returns `None` if no
/// propagated context is present.
///
/// For most use cases, prefer [`with_propagated_context`] which
/// automatically establishes the dcontext scope for async handlers.
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
/// If the message carries a propagated context (via interceptors), this
/// establishes a dcontext task-local scope so that `get_context` /
/// `set_context` work correctly inside the handler.
///
/// If no propagated context is found, the future runs without any
/// dcontext scope (a no-op passthrough).
///
/// # Usage
///
/// ```ignore
/// use dcontext_dactor::with_propagated_context;
/// use dactor::{async_trait, Actor, Handler, ActorContext, Message};
///
/// struct MyActor;
///
/// #[async_trait]
/// impl Handler<MyMessage> for MyActor {
///     async fn handle(&mut self, msg: MyMessage, ctx: &mut ActorContext) -> () {
///         with_propagated_context(ctx, async {
///             // dcontext is available here
///             let rid: RequestId = dcontext::get_context("request_id");
///             println!("handling with request_id = {:?}", rid);
///         }).await;
///     }
/// }
/// ```
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
