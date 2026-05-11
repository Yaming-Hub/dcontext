//! # Sample 5: Cross-Process Serialization
//!
//! Demonstrates serializing context to bytes/base64 for propagation
//! across process boundaries (e.g., HTTP headers, gRPC metadata, message queues).
//!
//! Usage: `cargo run --bin cross_process`

use base64::Engine as _;
use dcontext::{initialize, sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceContext {
    trace_id: String,
    span_id: String,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct AuthInfo {
    user_id: String,
    roles: Vec<String>,
}

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<TraceContext>("trace_context");
    builder.register::<AuthInfo>("auth_info");
    initialize(builder);

    // --- Sender side: serialize context ---
    sync_ctx::set_context(
        "trace_context",
        TraceContext {
            trace_id: "tid-abc-123".into(),
            span_id: "span-001".into(),
        },
    );
    sync_ctx::set_context(
        "auth_info",
        AuthInfo {
            user_id: "alice".into(),
            roles: vec!["admin".into(), "viewer".into()],
        },
    );

    println!("=== Sender ===");
    println!(
        "trace = {:?}",
        sync_ctx::get_context::<TraceContext>("trace_context").unwrap()
    );
    println!(
        "auth  = {:?}",
        sync_ctx::get_context::<AuthInfo>("auth_info").unwrap()
    );

    // Serialize to bytes (for binary protocols).
    let bytes = sync_ctx::serialize_context().unwrap();
    println!("\nSerialized to {} bytes", bytes.len());

    // Serialize to base64 string (for HTTP headers).
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    println!("Base64: {}...", &encoded[..40.min(encoded.len())]);

    // --- Receiver side: deserialize context ---
    println!("\n=== Receiver (bytes) ===");
    {
        let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
        println!(
            "trace = {:?}",
            sync_ctx::get_context::<TraceContext>("trace_context").unwrap()
        );
        println!(
            "auth  = {:?}",
            sync_ctx::get_context::<AuthInfo>("auth_info").unwrap()
        );
    }

    println!("\n=== Receiver (base64 string) ===");
    {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .unwrap();
        let _guard = sync_ctx::deserialize_context(&decoded).unwrap();
        println!(
            "trace = {:?}",
            sync_ctx::get_context::<TraceContext>("trace_context").unwrap()
        );
        println!(
            "auth  = {:?}",
            sync_ctx::get_context::<AuthInfo>("auth_info").unwrap()
        );
    }

    // Unknown keys on the receiver side are silently skipped.
    println!("\n=== Partial receiver (only trace_context registered) ===");
    // In a real scenario, the receiver process would only have registered
    // the context types it cares about. Unknown keys in the wire format
    // are silently ignored.
    println!("(In production, unregistered keys are skipped during deserialization)");
}
