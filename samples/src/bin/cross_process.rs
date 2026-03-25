//! # Sample 5: Cross-Process Serialization
//!
//! Demonstrates serializing context to bytes/base64 for propagation
//! across process boundaries (e.g., HTTP headers, gRPC metadata, message queues).
//!
//! Usage: `cargo run --bin cross_process`

use dcontext::{
    RegistryBuilder, initialize, set_context, get_context, scope,
    serialize_context, deserialize_context,
    serialize_context_string, deserialize_context_string,
};
use serde::{Serialize, Deserialize};

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
    set_context("trace_context", TraceContext {
        trace_id: "tid-abc-123".into(),
        span_id: "span-001".into(),
    });
    set_context("auth_info", AuthInfo {
        user_id: "alice".into(),
        roles: vec!["admin".into(), "viewer".into()],
    });

    println!("=== Sender ===");
    println!("trace = {:?}", get_context::<TraceContext>("trace_context"));
    println!("auth  = {:?}", get_context::<AuthInfo>("auth_info"));

    // Serialize to bytes (for binary protocols).
    let bytes = serialize_context().unwrap();
    println!("\nSerialized to {} bytes", bytes.len());

    // Serialize to base64 string (for HTTP headers).
    let encoded = serialize_context_string().unwrap();
    println!("Base64: {}...", &encoded[..40.min(encoded.len())]);

    // --- Receiver side: deserialize context ---
    println!("\n=== Receiver (bytes) ===");
    scope(|| {
        let _guard = deserialize_context(&bytes).unwrap();
        println!("trace = {:?}", get_context::<TraceContext>("trace_context"));
        println!("auth  = {:?}", get_context::<AuthInfo>("auth_info"));
    });

    println!("\n=== Receiver (base64 string) ===");
    scope(|| {
        let _guard = deserialize_context_string(&encoded).unwrap();
        println!("trace = {:?}", get_context::<TraceContext>("trace_context"));
        println!("auth  = {:?}", get_context::<AuthInfo>("auth_info"));
    });

    // Unknown keys on the receiver side are silently skipped.
    println!("\n=== Partial receiver (only trace_context registered) ===");
    // In a real scenario, the receiver process would only have registered
    // the context types it cares about. Unknown keys in the wire format
    // are silently ignored.
    println!("(In production, unregistered keys are skipped during deserialization)");
}
