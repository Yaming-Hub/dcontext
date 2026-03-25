//! # Sample 12: Versioned Registration
//!
//! Demonstrates `register_with` + `.version()` for wire format evolution. When a context
//! struct's schema changes between releases, per-key versioning ensures that
//! nodes running different versions can detect mismatches instead of silently
//! deserializing garbage.
//!
//! Usage: `cargo run --bin versioned_registration`

use dcontext::{
    register_with, initialize, set_context, get_context, scope,
    serialize_context, deserialize_context,
};
use serde::{Serialize, Deserialize};

/// Version 1 of the trace context — original schema.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceContextV1 {
    trace_id: String,
}

/// Version 2 adds a span_id field.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceContextV2 {
    trace_id: String,
    span_id: String,
}

fn main() {
    println!("=== Versioned Registration ===\n");

    // Register all keys upfront, then freeze the registry.
    register_with::<TraceContextV2>("trace_ctx_v2_same", |o| o.version(2));
    register_with::<TraceContextV1>("trace_ctx_v1_demo", |o| o.version(1));
    register_with::<TraceContextV2>("trace_ctx_v2_demo", |o| o.version(2));
    register_with::<TraceContextV1>("trace_ctx_unknown", |o| o.version(1));
    initialize();

    // --- Scenario 1: Same version on both sides ---
    println!("1. Same version (v2 → v2): success");
    {
        // Sender registers v2 and serializes.
        set_context("trace_ctx_v2_same", TraceContextV2 {
            trace_id: "tid-001".into(),
            span_id: "span-42".into(),
        });
        let bytes = serialize_context().unwrap();

        // Receiver also has v2 registered — deserialize succeeds.
        scope(|| {
            let _guard = deserialize_context(&bytes).unwrap();
            let ctx: TraceContextV2 = get_context("trace_ctx_v2_same");
            println!("   trace_id = {}, span_id = {}", ctx.trace_id, ctx.span_id);
        });
    }

    // --- Scenario 2: Version mismatch detection ---
    println!("\n2. Version mismatch: how it works");
    println!("   In production, sender and receiver are separate processes.");
    println!("   If sender serializes with version=2 but receiver has version=1,");
    println!("   deserialization returns DeserializationFailed with a clear message.");
    println!("   (Cannot demonstrate in-process because the global registry is shared.)");

    // Show the version is embedded in the wire format.
    {
        set_context("trace_ctx_v1_demo", TraceContextV1 {
            trace_id: "tid-v1".into(),
        });

        set_context("trace_ctx_v2_demo", TraceContextV2 {
            trace_id: "tid-v2".into(),
            span_id: "span-v2".into(),
        });

        let bytes = serialize_context().unwrap();
        println!("   Serialized both v1 and v2 keys into {} bytes", bytes.len());
        println!("   Each WireEntry includes the key_version for mismatch detection.");
    }

    // --- Scenario 3: Unknown key on receiver ---
    println!("\n3. Unknown key on receiver: silently skipped");
    {
        set_context("trace_ctx_unknown", TraceContextV1 {
            trace_id: "tid-003".into(),
        });
        let bytes = serialize_context().unwrap();

        // Receiver doesn't have "trace_ctx_unknown" registered at all.
        let handle = std::thread::spawn(move || {
            // Don't register the key — deserialization should skip it.
            let result = deserialize_context(&bytes);
            match result {
                Ok(_guard) => println!("   Deserialized OK (unknown keys skipped)"),
                Err(e) => println!("   Error: {}", e),
            }
        });
        handle.join().unwrap();
    }

    println!("\nDone! Versioned registration enables safe rolling upgrades.");
}
