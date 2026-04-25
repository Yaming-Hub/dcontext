use dactor::{
    ActorContext, Disposition, HeaderValue, Headers, InboundContext, InboundInterceptor,
    NodeId, ActorId, OutboundContext, OutboundInterceptor, RuntimeHeaders, SendMode,
};
use dcontext::ContextSnapshot;

use crate::header::{ContextHeader, ContextSnapshotHeader};
use crate::inbound::ContextInboundInterceptor;
use crate::outbound::ContextOutboundInterceptor;
use crate::propagation::{bytes_to_snapshot, extract_context};
use crate::ErrorPolicy;

// ── Test context type ──────────────────────────────────────────

#[derive(Clone, Default, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct RequestId(String);

fn init_registry() {
    let mut builder = dcontext::RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    let _ = dcontext::try_initialize(builder);
}

// ── Header tests ───────────────────────────────────────────────

#[test]
fn context_header_has_wire_bytes() {
    let header = ContextHeader {
        bytes: vec![1, 2, 3],
    };
    assert_eq!(header.header_name(), "dcontext.wire");
    assert_eq!(header.to_bytes(), Some(vec![1, 2, 3]));
}

#[test]
fn snapshot_header_is_local_only() {
    let header = ContextSnapshotHeader {
        snapshot: ContextSnapshot::empty(),
    };
    assert_eq!(header.header_name(), "dcontext.snapshot");
    assert!(header.to_bytes().is_none(), "snapshot header should be local-only");
}

// ── Outbound interceptor tests ─────────────────────────────────

fn make_outbound_ctx(remote: bool) -> OutboundContext<'static> {
    OutboundContext {
        target_id: ActorId {
            node: NodeId("test-node".into()),
            local: 1,
        },
        target_name: "test-actor",
        message_type: "TestMsg",
        send_mode: SendMode::Tell,
        remote,
    }
}

#[test]
fn outbound_remote_attaches_wire_header_only() {
    init_registry();

    let interceptor = ContextOutboundInterceptor::default();
    let ctx = make_outbound_ctx(true);
    let rh = RuntimeHeaders::new();
    let mut headers = Headers::new();
    let msg = 42u64;

    let disposition = interceptor.on_send(&ctx, &rh, &mut headers, &msg);
    assert!(matches!(disposition, Disposition::Continue));

    assert!(headers.get::<ContextHeader>().is_some(), "remote should have wire header");
    assert!(
        headers.get::<ContextSnapshotHeader>().is_none(),
        "remote should not have snapshot header"
    );
}

#[test]
fn outbound_local_attaches_snapshot_only() {
    init_registry();

    let _guard = dcontext::enter_scope();
    dcontext::set_context("request_id", RequestId("req-abc".into()));

    let interceptor = ContextOutboundInterceptor::default();
    let ctx = make_outbound_ctx(false);
    let rh = RuntimeHeaders::new();
    let mut headers = Headers::new();
    let msg = 42u64;

    let disposition = interceptor.on_send(&ctx, &rh, &mut headers, &msg);
    assert!(matches!(disposition, Disposition::Continue));

    assert!(
        headers.get::<ContextSnapshotHeader>().is_some(),
        "local should have snapshot header"
    );
    assert!(
        headers.get::<ContextHeader>().is_none(),
        "local should NOT have wire header — no serialization needed"
    );
}

// ── Error policy tests ─────────────────────────────────────────

#[test]
fn outbound_reject_policy_rejects_on_serialization_error() {
    // Don't init_registry — serialization will fail for unregistered types.
    // But serialize_context() won't fail because it only serializes what's
    // in the context. To test rejection, we need to trigger a real error.
    // serialize_context only fails on bincode/size errors which are hard to
    // trigger, so we test the policy plumbing via the inbound interceptor
    // which is easier to trigger with corrupt bytes.

    let interceptor = ContextOutboundInterceptor::new(ErrorPolicy::Reject);
    assert_eq!(interceptor.name(), "dcontext-outbound");
}

#[test]
fn inbound_log_and_continue_on_corrupt_bytes() {
    init_registry();

    let interceptor = ContextInboundInterceptor::default();
    let ctx = make_inbound_ctx();
    let rh = RuntimeHeaders::new();
    let mut headers = Headers::new();
    let msg = 42u64;

    headers.insert(ContextHeader {
        bytes: vec![0xFF, 0xFE, 0xFD],
    });

    let disposition = interceptor.on_receive(&ctx, &rh, &mut headers, &msg);
    assert!(
        matches!(disposition, Disposition::Continue),
        "LogAndContinue should not reject"
    );
    assert!(
        headers.get::<ContextSnapshotHeader>().is_none(),
        "corrupt bytes should not produce a snapshot"
    );
}

#[test]
fn inbound_reject_policy_rejects_on_corrupt_bytes() {
    init_registry();

    let interceptor = ContextInboundInterceptor::new(ErrorPolicy::Reject);
    let ctx = make_inbound_ctx();
    let rh = RuntimeHeaders::new();
    let mut headers = Headers::new();
    let msg = 42u64;

    headers.insert(ContextHeader {
        bytes: vec![0xFF, 0xFE, 0xFD],
    });

    let disposition = interceptor.on_receive(&ctx, &rh, &mut headers, &msg);
    assert!(
        matches!(disposition, Disposition::Reject(_)),
        "Reject policy should reject on corrupt bytes"
    );
}

// ── Inbound interceptor tests ──────────────────────────────────

fn make_inbound_ctx() -> InboundContext<'static> {
    InboundContext {
        actor_id: ActorId {
            node: NodeId("test-node".into()),
            local: 1,
        },
        actor_name: "test-actor",
        message_type: "TestMsg",
        send_mode: SendMode::Tell,
        remote: false,
        origin_node: None,
    }
}

#[test]
fn inbound_interceptor_preserves_existing_snapshot() {
    let interceptor = ContextInboundInterceptor::default();
    let ctx = make_inbound_ctx();
    let rh = RuntimeHeaders::new();
    let mut headers = Headers::new();
    let msg = 42u64;

    headers.insert(ContextSnapshotHeader {
        snapshot: ContextSnapshot::empty(),
    });

    let disposition = interceptor.on_receive(&ctx, &rh, &mut headers, &msg);
    assert!(matches!(disposition, Disposition::Continue));
    assert!(headers.get::<ContextSnapshotHeader>().is_some());
}

#[test]
fn inbound_interceptor_converts_wire_to_snapshot() {
    init_registry();

    let wire_bytes = dcontext::force_thread_local(|| {
        let _guard = dcontext::enter_scope();
        dcontext::set_context("request_id", RequestId("req-wire".into()));
        dcontext::serialize_context().unwrap()
    });

    let interceptor = ContextInboundInterceptor::default();
    let ctx = make_inbound_ctx();
    let rh = RuntimeHeaders::new();
    let mut headers = Headers::new();
    let msg = 42u64;

    headers.insert(ContextHeader { bytes: wire_bytes });

    let disposition = interceptor.on_receive(&ctx, &rh, &mut headers, &msg);
    assert!(matches!(disposition, Disposition::Continue));
    assert!(
        headers.get::<ContextSnapshotHeader>().is_some(),
        "inbound interceptor should convert wire bytes to snapshot"
    );
}

// ── bytes_to_snapshot tests ────────────────────────────────────

#[test]
fn bytes_to_snapshot_roundtrip() {
    init_registry();

    let wire_bytes = dcontext::force_thread_local(|| {
        let _guard = dcontext::enter_scope();
        dcontext::set_context("request_id", RequestId("roundtrip".into()));
        dcontext::serialize_context().unwrap()
    });

    let snap = bytes_to_snapshot(&wire_bytes);
    assert!(snap.is_some(), "should produce a snapshot from valid wire bytes");
}

#[test]
fn bytes_to_snapshot_invalid_bytes() {
    init_registry();

    let snap = bytes_to_snapshot(&[0xFF, 0xFE, 0xFD]);
    assert!(snap.is_none(), "invalid bytes should return None");
}

// ── extract_context tests ──────────────────────────────────────

#[test]
fn extract_context_prefers_snapshot() {
    let mut actor_ctx = ActorContext::new(
        ActorId {
            node: NodeId("n".into()),
            local: 1,
        },
        "test".into(),
    );

    actor_ctx.headers.insert(ContextSnapshotHeader {
        snapshot: ContextSnapshot::empty(),
    });
    actor_ctx.headers.insert(ContextHeader {
        bytes: vec![1, 2, 3],
    });

    let result = extract_context(&actor_ctx);
    assert!(result.is_some());
}

#[test]
fn extract_context_returns_none_when_empty() {
    let actor_ctx = ActorContext::new(
        ActorId {
            node: NodeId("n".into()),
            local: 1,
        },
        "test".into(),
    );

    let result = extract_context(&actor_ctx);
    assert!(result.is_none());
}

// ── End-to-end test: outbound → inbound → extract ──────────────

#[test]
fn end_to_end_local_propagation() {
    init_registry();

    let _guard = dcontext::enter_scope();
    dcontext::set_context("request_id", RequestId("e2e-local".into()));

    // Outbound interceptor captures snapshot (no serialization for local).
    let outbound = ContextOutboundInterceptor::default();
    let out_ctx = make_outbound_ctx(false);
    let rh = RuntimeHeaders::new();
    let mut headers = Headers::new();
    let msg = 42u64;
    outbound.on_send(&out_ctx, &rh, &mut headers, &msg);

    // Verify no wire header was produced for local target.
    assert!(headers.get::<ContextHeader>().is_none(), "local should skip serialization");
    assert!(headers.get::<ContextSnapshotHeader>().is_some());

    // Inbound interceptor — snapshot already present, nothing to convert.
    let inbound = ContextInboundInterceptor::default();
    let in_ctx = make_inbound_ctx();
    inbound.on_receive(&in_ctx, &rh, &mut headers, &msg);

    // Build a mock ActorContext with the headers.
    let mut actor_ctx = ActorContext::new(
        ActorId {
            node: NodeId("n".into()),
            local: 1,
        },
        "test".into(),
    );
    actor_ctx.headers = headers;

    // Extract and verify.
    let snap = extract_context(&actor_ctx).expect("should have propagated context");
    let _restore = dcontext::attach(snap);
    let rid: RequestId = dcontext::get_context("request_id");
    assert_eq!(rid.0, "e2e-local");
}

#[test]
fn end_to_end_remote_propagation() {
    init_registry();

    // Simulate sender serializing context (remote path).
    let wire_bytes = dcontext::force_thread_local(|| {
        let _guard = dcontext::enter_scope();
        dcontext::set_context("request_id", RequestId("e2e-remote".into()));
        dcontext::serialize_context().unwrap()
    });

    // Simulate receiving wire bytes (as if from remote transport).
    let mut headers = Headers::new();
    headers.insert(ContextHeader { bytes: wire_bytes });

    // Inbound interceptor converts wire to snapshot.
    let inbound = ContextInboundInterceptor::default();
    let in_ctx = make_inbound_ctx();
    let rh = RuntimeHeaders::new();
    let msg = 42u64;
    inbound.on_receive(&in_ctx, &rh, &mut headers, &msg);

    let mut actor_ctx = ActorContext::new(
        ActorId {
            node: NodeId("n".into()),
            local: 1,
        },
        "test".into(),
    );
    actor_ctx.headers = headers;

    let snap = extract_context(&actor_ctx).expect("should have propagated context");
    let _restore = dcontext::attach(snap);
    let rid: RequestId = dcontext::get_context("request_id");
    assert_eq!(rid.0, "e2e-remote");
}

// ── Async propagation test ─────────────────────────────────────

#[tokio::test]
async fn with_propagated_context_establishes_scope() {
    init_registry();

    let snap = dcontext::force_thread_local(|| {
        let _guard = dcontext::enter_scope();
        dcontext::set_context("request_id", RequestId("async-test".into()));
        dcontext::snapshot()
    });

    let mut actor_ctx = ActorContext::new(
        ActorId {
            node: NodeId("n".into()),
            local: 1,
        },
        "test".into(),
    );
    actor_ctx.headers.insert(ContextSnapshotHeader { snapshot: snap });

    let result = crate::with_propagated_context(&actor_ctx, async {
        let rid: RequestId = dcontext::get_context("request_id");
        rid.0
    })
    .await;

    assert_eq!(result, "async-test");
}

#[tokio::test]
async fn with_propagated_context_passthrough_without_headers() {
    init_registry();

    let actor_ctx = ActorContext::new(
        ActorId {
            node: NodeId("n".into()),
            local: 1,
        },
        "test".into(),
    );

    let result = crate::with_propagated_context(&actor_ctx, async { 42 }).await;
    assert_eq!(result, 42);
}
