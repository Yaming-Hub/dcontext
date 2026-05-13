//! # Sample: Unified Context — Cross-Process Serialization
//!
//! Demonstrates serializing a captured snapshot, transmitting it, and restoring
//! it in async or sync receivers with the new API.
//!
//! Usage: `cargo run --bin dual_cross_process`

use base64::Engine as _;
use dcontext::{
    attach_snapshot, get_context_variable, initialize, push_scope, scope_chain,
    set_context_variable, ContextFutureExt, ContextSnapshot, RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct ServiceName(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestPath(String);

#[tokio::main]
async fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<TraceId>("trace_id");
    builder.register::<ServiceName>("service_name");
    builder.register::<RequestPath>("request_path");
    initialize(builder);

    println!("=== Cross-Process Context Propagation ===\n");
    println!("--- Sender (API Gateway) ---");

    set_context_variable("trace_id", TraceId("trace-xyz-789".into()));
    set_context_variable("service_name", ServiceName("api-gateway".into()));
    set_context_variable("request_path", RequestPath("/api/v1/orders".into()));

    let tid: TraceId = get_context_variable("trace_id").unwrap();
    let svc: ServiceName = get_context_variable("service_name").unwrap();
    println!("  trace_id     = {:?}", tid.0);
    println!("  service_name = {:?}", svc.0);

    let wire_bytes = dcontext::capture().serialize().unwrap();
    let wire_string = base64::engine::general_purpose::STANDARD.encode(&wire_bytes);

    println!("  Serialized: {} bytes", wire_bytes.len());
    println!("  Base64: {}...", &wire_string[..40.min(wire_string.len())]);
    println!("\n  [--- network transmission ---]\n");

    println!("--- Receiver (Order Service, async) ---");
    let receiver_snap = ContextSnapshot::deserialize(&wire_bytes).unwrap();
    async move {
        let tid: Option<TraceId> = get_context_variable("trace_id");
        let svc: Option<ServiceName> = get_context_variable("service_name");

        println!("  [async] trace_id     = {:?}", tid.map(|t| t.0));
        println!("  [async] service_name = {:?}", svc.map(|s| s.0));
        println!("  [async] scope_chain  = {:?}", scope_chain());

        async {
            println!("  [async] in scope: chain = {:?}", scope_chain());
            println!("\n--- Forwarding to downstream (Inventory Service) ---");
        }
        .scope("process_order")
        .await;
    }
    .scope("order_service")
    .attach(receiver_snap)
    .await;

    println!("\n--- Receiver (Legacy Worker, sync) ---");
    std::thread::spawn(move || {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&wire_string)
            .unwrap();
        let snap = ContextSnapshot::deserialize(&bytes).unwrap();
        let _guard = attach_snapshot(snap);
        let tid: TraceId = get_context_variable("trace_id").unwrap();
        let path: RequestPath = get_context_variable("request_path").unwrap();
        println!("  [sync] trace_id     = {:?}", tid.0);
        println!("  [sync] request_path = {:?}", path.0);

        let _guard = push_scope("legacy_handler");
        println!("  [sync] scope_chain = {:?}", scope_chain());
    })
    .join()
    .unwrap();

    println!("\nDone!");
}
