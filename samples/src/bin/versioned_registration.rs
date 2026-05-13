//! # Sample 12: Versioned Registration
//!
//! Demonstrates `register_with` + `.version()` for wire format evolution.
//!
//! Usage: `cargo run --bin versioned_registration`

use dcontext::{
    attach_snapshot, capture, get_context_variable, initialize, set_context_variable,
    ContextSnapshot, RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceContextV1 {
    trace_id: String,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceContextV2 {
    trace_id: String,
    span_id: String,
}

fn main() {
    println!("=== Versioned Registration ===\n");

    let mut builder = RegistryBuilder::new();
    builder.register_with::<TraceContextV2>("trace_ctx_v2_same", |o| o.version(2));
    builder.register_with::<TraceContextV1>("trace_ctx_v1_demo", |o| o.version(1));
    builder.register_with::<TraceContextV2>("trace_ctx_v2_demo", |o| o.version(2));
    builder.register_with::<TraceContextV1>("trace_ctx_unknown", |o| o.version(1));
    initialize(builder);

    println!("1. Same version (v2 → v2): success");
    set_context_variable(
        "trace_ctx_v2_same",
        TraceContextV2 {
            trace_id: "tid-001".into(),
            span_id: "span-42".into(),
        },
    );
    let bytes = capture().serialize().unwrap();
    {
        let snap = ContextSnapshot::deserialize(&bytes).unwrap();
        let _guard = attach_snapshot(snap);
        let ctx: TraceContextV2 = get_context_variable("trace_ctx_v2_same").unwrap();
        println!("   trace_id = {}, span_id = {}", ctx.trace_id, ctx.span_id);
    }

    println!("\n2. Version mismatch: how it works");
    println!("   In production, sender and receiver are separate processes.");
    println!("   If sender serializes with version=2 but receiver has version=1,");
    println!("   deserialization returns a clear mismatch error.");

    set_context_variable(
        "trace_ctx_v1_demo",
        TraceContextV1 {
            trace_id: "tid-v1".into(),
        },
    );
    set_context_variable(
        "trace_ctx_v2_demo",
        TraceContextV2 {
            trace_id: "tid-v2".into(),
            span_id: "span-v2".into(),
        },
    );
    let bytes = capture().serialize().unwrap();
    println!(
        "   Serialized both v1 and v2 keys into {} bytes",
        bytes.len()
    );
    println!("   Each wire entry includes the key_version for mismatch detection.");

    println!("\n3. Unknown key on receiver: silently skipped");
    set_context_variable(
        "trace_ctx_unknown",
        TraceContextV1 {
            trace_id: "tid-003".into(),
        },
    );
    let bytes = capture().serialize().unwrap();
    let handle = std::thread::spawn(move || {
        let result = ContextSnapshot::deserialize(&bytes);
        match result {
            Ok(_snap) => println!(
                "   Deserialized OK (unknown keys are skipped by receivers that omit them)"
            ),
            Err(e) => println!("   Error: {}", e),
        }
    });
    handle.join().unwrap();

    println!("\nDone! Versioned registration enables safe rolling upgrades.");
}
