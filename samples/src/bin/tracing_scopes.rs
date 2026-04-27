//! # dcontext-tracing: Automatic Scoping via Tracing Spans
//!
//! This sample demonstrates all three levels of dcontext-tracing integration:
//! 1. Auto-scoping — every span creates a dcontext scope
//! 2. Field mapping — span fields automatically become context values
//! 3. Span info — span metadata available as context
//!
//! Run with: `cargo run --bin tracing_scopes`

use dcontext_tracing::{DcontextLayer, FromFieldValue, SpanInfo, SPAN_INFO_KEY};
use tracing::Instrument;
use tracing_subscriber::prelude::*;

// ── Context types ──────────────────────────────────────────────

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
struct RequestId(String);

impl FromFieldValue for RequestId {
    fn from_str_value(s: &str) -> Option<Self> {
        Some(RequestId(s.to_string()))
    }
}

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
struct UserId(String);

impl FromFieldValue for UserId {
    fn from_str_value(s: &str) -> Option<Self> {
        Some(UserId(s.to_string()))
    }
}

// ── Setup ──────────────────────────────────────────────────────

fn init() -> tracing::subscriber::DefaultGuard {
    // 1. Register context types with dcontext
    let mut builder = dcontext::RegistryBuilder::new();
    builder.register::<String>("status");
    builder.register::<RequestId>("request_id");
    builder.register::<UserId>("user_id");
    builder.register::<SpanInfo>(SPAN_INFO_KEY);
    dcontext::initialize(builder);

    // 2. Configure the tracing layer with field mappings and span info
    let layer = DcontextLayer::builder()
        .map_field::<RequestId>("request_id")
        .map_field::<UserId>("user_id")
        .include_span_info()
        .build();

    tracing_subscriber::registry()
        .with(layer)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .set_default()
}

// ── Demo functions ─────────────────────────────────────────────

/// Level 1: Automatic scoping — context values follow span lifecycle
fn demo_auto_scoping() {
    println!("\n=== Level 1: Automatic Scoping ===\n");

    dcontext::set_context("status", "root".to_string());
    println!("  Before span: status = {:?}", dcontext::get_context::<String>("status"));

    {
        let _span = tracing::info_span!("outer_operation").entered();
        dcontext::set_context("status", "in-outer-span".to_string());
        println!("  In outer span: status = {:?}", dcontext::get_context::<String>("status"));

        {
            let _span = tracing::info_span!("inner_operation").entered();
            dcontext::set_context("status", "in-inner-span".to_string());
            println!("  In inner span: status = {:?}", dcontext::get_context::<String>("status"));
        }

        // Inner span exited — context reverted
        println!("  After inner exits: status = {:?}", dcontext::get_context::<String>("status"));
    }

    // Outer span exited — context reverted to root
    println!("  After outer exits: status = {:?}", dcontext::get_context::<String>("status"));
}

/// Level 2: Field mapping — span fields become context values
fn demo_field_mapping() {
    println!("\n=== Level 2: Field Mapping ===\n");

    {
        let _span = tracing::info_span!(
            "handle_request",
            request_id = "req-abc-123",
            user_id = "user-42"
        )
        .entered();

        let rid: RequestId = dcontext::get_context("request_id");
        let uid: UserId = dcontext::get_context("user_id");
        println!("  request_id = {:?}", rid.0);
        println!("  user_id = {:?}", uid.0);
    }

    // After span — values are reverted to defaults
    let rid: RequestId = dcontext::get_context("request_id");
    println!("  After span: request_id = {:?} (empty = reverted)", rid.0);
}

/// Level 3: Span info — metadata available as context
fn demo_span_info() {
    println!("\n=== Level 3: Span Info ===\n");

    {
        let _span = tracing::warn_span!("validate_order").entered();
        let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
        println!("  name   = {:?}", info.name);
        println!("  target = {:?}", info.target);
        println!("  level  = {:?}", info.level);
    }
}

/// Async demo — field mapping works with Instrument
async fn demo_async() {
    println!("\n=== Async with Instrument ===\n");

    async fn process_order(order_id: &str) {
        let rid: RequestId = dcontext::force_thread_local(|| dcontext::get_context("request_id"));
        let info: SpanInfo =
            dcontext::force_thread_local(|| dcontext::get_context(SPAN_INFO_KEY));
        println!(
            "  Processing order {} — request={}, span={}",
            order_id, rid.0, info.name
        );
    }

    process_order("ORD-001")
        .instrument(tracing::info_span!("process_order", request_id = "req-async-789"))
        .await;
}

// ── Main ───────────────────────────────────────────────────────

fn main() {
    let _guard = init();

    println!("dcontext-tracing Sample: Automatic Scoping via Tracing Spans");
    println!("=============================================================");

    // Run sync demos outside tokio runtime
    demo_auto_scoping();
    demo_field_mapping();
    demo_span_info();

    // Run async demo inside tokio runtime
    tokio::runtime::Runtime::new().unwrap().block_on(demo_async());

    println!("\nDone!");
}
