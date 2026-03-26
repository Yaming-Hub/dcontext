//! # Sample 13: Version Migration
//!
//! Demonstrates `register_migration` for automatic wire format migration.
//! When a context struct's schema evolves, nodes can register migration
//! functions that convert old wire versions to the current type during
//! deserialization.
//!
//! Usage: `cargo run --bin version_migration`

use dcontext::{
    RegistryBuilder, initialize, set_context, get_context,
    scope, serialize_context, deserialize_context, make_wire_bytes,
};
use serde::{Serialize, Deserialize};

/// Version 1: original schema with just a trace_id.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceContextV1 {
    trace_id: String,
}

/// Version 2: adds span_id and a sampling flag.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceContextV2 {
    trace_id: String,
    span_id: String,
    sampled: bool,
}

fn main() {
    println!("=== Version Migration ===\n");

    // Register the CURRENT version (V2) of the context type.
    let mut builder = RegistryBuilder::new();
    builder.register_with::<TraceContextV2>("trace_ctx", |o| o.version(2));

    // Register a migration from V1 → V2. When wire bytes arrive with
    // key_version=1, they are deserialized as TraceContextV1, then
    // converted to TraceContextV2 via this function.
    builder.register_migration::<TraceContextV1, TraceContextV2>("trace_ctx", 1, |v1| {
        TraceContextV2 {
            trace_id: v1.trace_id,
            span_id: String::new(),  // V1 didn't have span_id
            sampled: true,           // default: sampled
        }
    });
    initialize(builder);

    // --- Scenario 1: Current version roundtrip (V2 → V2) ---
    println!("1. Current version roundtrip (V2 → V2):");
    set_context("trace_ctx", TraceContextV2 {
        trace_id: "tid-current".into(),
        span_id: "span-42".into(),
        sampled: false,
    });
    let bytes_v2 = serialize_context().unwrap();

    scope(|| {
        let _guard = deserialize_context(&bytes_v2).unwrap();
        let ctx: TraceContextV2 = get_context("trace_ctx");
        println!("   trace_id = {}", ctx.trace_id);
        println!("   span_id  = {}", ctx.span_id);
        println!("   sampled  = {}", ctx.sampled);
    });

    // --- Scenario 2: Receiving V1 bytes → auto-migrated to V2 ---
    println!("\n2. Receiving old V1 bytes → auto-migrated to V2:");
    println!("   (Simulating bytes from a node running the old schema)");

    // To simulate V1 wire bytes, we construct them using make_wire_bytes.
    // In production, these would come from serialize_context() on a V1 sender.
    let v1_wire = {
        let v1 = TraceContextV1 { trace_id: "tid-from-old-node".into() };
        let v1_bytes = bincode::serialize(&v1).unwrap();
        make_wire_bytes("trace_ctx", 1, &v1_bytes)
    };

    scope(|| {
        let _guard = deserialize_context(&v1_wire).unwrap();
        let ctx: TraceContextV2 = get_context("trace_ctx");
        println!("   trace_id = {} (preserved from V1)", ctx.trace_id);
        println!("   span_id  = {:?} (default — V1 didn't have it)", ctx.span_id);
        println!("   sampled  = {} (default — V1 didn't have it)", ctx.sampled);
    });

    // --- Scenario 3: Multiple versions coexist ---
    println!("\n3. Both V1 and V2 deserializers registered:");
    println!("   The receiver can handle bytes from any known version.");
    println!("   Unknown versions return a clear error listing supported versions.");

    println!("\nDone! Version migration enables safe rolling upgrades.");
}
