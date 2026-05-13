//! # Sample 13: Version Migration
//!
//! Demonstrates `register_migration` for automatic wire format migration.
//!
//! Usage: `cargo run --bin version_migration`

use dcontext::{
    attach_snapshot, capture, get_context_variable, initialize, make_wire_bytes,
    set_context_variable, ContextSnapshot, RegistryBuilder,
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
    sampled: bool,
}

fn main() {
    println!("=== Version Migration ===\n");

    let mut builder = RegistryBuilder::new();
    builder.register_with::<TraceContextV2>("trace_ctx", |o| o.version(2));
    builder.register_migration::<TraceContextV1, TraceContextV2>("trace_ctx", 1, |v1| {
        TraceContextV2 {
            trace_id: v1.trace_id,
            span_id: String::new(),
            sampled: true,
        }
    });
    initialize(builder);

    println!("1. Current version roundtrip (V2 → V2):");
    set_context_variable(
        "trace_ctx",
        TraceContextV2 {
            trace_id: "tid-current".into(),
            span_id: "span-42".into(),
            sampled: false,
        },
    );
    let bytes_v2 = capture().serialize().unwrap();

    {
        let snap = ContextSnapshot::deserialize(&bytes_v2).unwrap();
        let _guard = attach_snapshot(snap);
        let ctx: TraceContextV2 = get_context_variable("trace_ctx").unwrap();
        println!("   trace_id = {}", ctx.trace_id);
        println!("   span_id  = {}", ctx.span_id);
        println!("   sampled  = {}", ctx.sampled);
    }

    println!("\n2. Receiving old V1 bytes → auto-migrated to V2:");
    println!("   (Simulating bytes from a node running the old schema)");

    let v1_wire = {
        let v1 = TraceContextV1 {
            trace_id: "tid-from-old-node".into(),
        };
        let v1_bytes = bincode::serialize(&v1).unwrap();
        make_wire_bytes("trace_ctx", 1, &v1_bytes)
    };

    {
        let snap = ContextSnapshot::deserialize(&v1_wire).unwrap();
        let _guard = attach_snapshot(snap);
        let ctx: TraceContextV2 = get_context_variable("trace_ctx").unwrap();
        println!("   trace_id = {} (preserved from V1)", ctx.trace_id);
        println!(
            "   span_id  = {:?} (default — V1 didn't have it)",
            ctx.span_id
        );
        println!(
            "   sampled  = {} (default — V1 didn't have it)",
            ctx.sampled
        );
    }

    println!("\n3. Both V1 and V2 deserializers registered:");
    println!("   The receiver can handle bytes from any known version.");
    println!("   Unknown versions return a clear error listing supported versions.");
    println!("\nDone! Version migration enables safe rolling upgrades.");
}
