//! # Sample: Dual-Context — Cross-Process Serialization
//!
//! Demonstrates serializing context from `async_ctx`, transmitting it
//! (simulated), and restoring it in a remote process using either
//! `async_ctx::with_context` (for async receivers) or `sync_ctx::restore`
//! (for sync receivers).
//!
//! This sample shows the full flow:
//! 1. Sender: capture snapshot → serialize to bytes/base64
//! 2. Transport: simulate network transmission
//! 3. Receiver: deserialize → restore context
//!
//! Usage: `cargo run --bin dual_cross_process`

use base64::Engine as _;
use dcontext::{async_ctx, initialize, sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct ServiceName(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestPath(String);

#[tokio::main]
async fn main() {
    // Both sender and receiver must register the same types.
    // In production, this is typically in a shared library.
    let mut builder = RegistryBuilder::new();
    builder.register::<TraceId>("trace_id");
    builder.register::<ServiceName>("service_name");
    builder.register::<RequestPath>("request_path");
    initialize(builder);

    println!("=== Cross-Process Context Propagation ===\n");

    // ══════════════════════════════════════════════════════════
    //  Sender Side (e.g., API Gateway)
    // ══════════════════════════════════════════════════════════
    println!("--- Sender (API Gateway) ---");

    // Set context using the traditional registered-key API
    // (required for serialization — async_ctx values are untyped)
    sync_ctx::set_context("trace_id", TraceId("trace-xyz-789".into()));
    sync_ctx::set_context("service_name", ServiceName("api-gateway".into()));
    sync_ctx::set_context("request_path", RequestPath("/api/v1/orders".into()));

    let tid: TraceId = sync_ctx::get_context("trace_id").unwrap();
    let svc: ServiceName = sync_ctx::get_context("service_name").unwrap();
    println!("  trace_id     = {:?}", tid.0);
    println!("  service_name = {:?}", svc.0);

    // Serialize using the registered-key system
    let wire_bytes = sync_ctx::serialize_context().unwrap();
    let wire_string = base64::engine::general_purpose::STANDARD.encode(&wire_bytes);

    println!("  Serialized: {} bytes", wire_bytes.len());
    println!("  Base64: {}...", &wire_string[..40.min(wire_string.len())]);

    // ══════════════════════════════════════════════════════════
    //  Transport (simulate network)
    // ══════════════════════════════════════════════════════════
    println!("\n  [--- network transmission ---]\n");

    // ══════════════════════════════════════════════════════════
    //  Receiver Side — Async Service (e.g., Order Service)
    // ══════════════════════════════════════════════════════════
    println!("--- Receiver (Order Service, async) ---");

    // Deserialize into the traditional context system, then snapshot
    // for use in async_ctx
    let receiver_snap = {
        let _scope = sync_ctx::enter_named_scope("order_service");
        let _guard = sync_ctx::deserialize_context(&wire_bytes).unwrap();

        // Now the registered keys are populated
        let tid: TraceId = sync_ctx::get_context("trace_id").unwrap();
        println!("  [registered] trace_id = {:?}", tid.0);
        println!("  [registered] scope_chain = {:?}", sync_ctx::scope_chain());

        // Take a snapshot for use in async_ctx
        sync_ctx::snapshot()
    };

    // Use the snapshot in async context
    async_ctx::with_context(receiver_snap, async {
        let tid: Option<TraceId> = async_ctx::get_context("trace_id");
        let svc: Option<ServiceName> = async_ctx::get_context("service_name");
        let chain = async_ctx::scope_chain();

        println!("  [async_ctx] trace_id     = {:?}", tid.map(|t| t.0));
        println!("  [async_ctx] service_name = {:?}", svc.map(|s| s.0));
        println!("  [async_ctx] scope_chain  = {:?}", chain);

        // Process the request in an async scope
        async_ctx::scope("process_order", async {
            println!(
                "  [async_ctx] in scope: chain = {:?}",
                async_ctx::scope_chain()
            );

            // Simulate downstream call — serialize again for next hop
            println!("\n--- Forwarding to downstream (Inventory Service) ---");
            // In a real app, you'd serialize the registered context here
        })
        .await;
    })
    .await;

    // ══════════════════════════════════════════════════════════
    //  Receiver Side — Sync Service (e.g., Legacy worker)
    // ══════════════════════════════════════════════════════════
    println!("\n--- Receiver (Legacy Worker, sync) ---");

    // For sync receivers, deserialize and use sync_ctx
    std::thread::spawn(move || {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&wire_string)
            .unwrap();
        let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
        let tid: TraceId = sync_ctx::get_context("trace_id").unwrap();
        let path: RequestPath = sync_ctx::get_context("request_path").unwrap();
        println!("  [sync] trace_id     = {:?}", tid.0);
        println!("  [sync] request_path = {:?}", path.0);

        // Or use sync_ctx directly for scope management
        let _guard = sync_ctx::push_scope("legacy_handler");
        println!("  [sync_ctx] scope_chain = {:?}", sync_ctx::scope_chain());
    })
    .join()
    .unwrap();

    println!("\nDone!");
}
