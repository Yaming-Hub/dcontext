//! # Sample 5: Cross-Process Serialization
//!
//! Demonstrates serializing context to bytes/base64 for propagation
//! across process boundaries (e.g., HTTP headers, gRPC metadata, message queues).
//!
//! Usage: `cargo run --bin cross_process`

use base64::Engine as _;
use dcontext::{
    attach_snapshot, capture, get_context_variable, initialize, set_context_variable,
    ContextSnapshot, RegistryBuilder,
};
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

    set_context_variable(
        "trace_context",
        TraceContext {
            trace_id: "tid-abc-123".into(),
            span_id: "span-001".into(),
        },
    );
    set_context_variable(
        "auth_info",
        AuthInfo {
            user_id: "alice".into(),
            roles: vec!["admin".into(), "viewer".into()],
        },
    );

    println!("=== Sender ===");
    println!(
        "trace = {:?}",
        get_context_variable::<TraceContext>("trace_context").unwrap()
    );
    println!(
        "auth  = {:?}",
        get_context_variable::<AuthInfo>("auth_info").unwrap()
    );

    let bytes = capture().serialize().unwrap();
    println!("\nSerialized to {} bytes", bytes.len());

    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    println!("Base64: {}...", &encoded[..40.min(encoded.len())]);

    println!("\n=== Receiver (bytes) ===");
    {
        let snap = ContextSnapshot::deserialize(&bytes).unwrap();
        let _guard = attach_snapshot(snap);
        println!(
            "trace = {:?}",
            get_context_variable::<TraceContext>("trace_context").unwrap()
        );
        println!(
            "auth  = {:?}",
            get_context_variable::<AuthInfo>("auth_info").unwrap()
        );
    }

    println!("\n=== Receiver (base64 string) ===");
    {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .unwrap();
        let snap = ContextSnapshot::deserialize(&decoded).unwrap();
        let _guard = attach_snapshot(snap);
        println!(
            "trace = {:?}",
            get_context_variable::<TraceContext>("trace_context").unwrap()
        );
        println!(
            "auth  = {:?}",
            get_context_variable::<AuthInfo>("auth_info").unwrap()
        );
    }

    println!("\n=== Partial receiver (only trace_context registered) ===");
    println!("(In production, unregistered keys are skipped during deserialization)");
}
