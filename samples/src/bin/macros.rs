//! # Sample 8: Macros — register_contexts! and with_scope!
//!
//! Demonstrates the convenience macros for bulk registration
//! and scoped context setting.
//!
//! Usage: `cargo run --bin macros`

use dcontext::{with_scope, get_context, set_context, RegistryBuilder};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct SpanId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TenantId(String);

fn main() {
    // Bulk registration with a single macro call.
    let mut builder = RegistryBuilder::new();
    dcontext::register_contexts!(builder, {
        "trace_id"  => TraceId,
        "span_id"   => SpanId,
        "tenant_id" => TenantId,
    });
    dcontext::initialize(builder);

    set_context("trace_id", TraceId("trace-root".into()));
    set_context("tenant_id", TenantId("acme-corp".into()));

    println!("=== Before with_scope! ===");
    println!("trace_id  = {:?}", get_context::<TraceId>("trace_id"));
    println!("span_id   = {:?}", get_context::<SpanId>("span_id"));
    println!("tenant_id = {:?}", get_context::<TenantId>("tenant_id"));

    // with_scope! sets multiple values and runs a block in a new scope.
    with_scope! {
        "span_id"  => SpanId("span-handler".into()),
        "trace_id" => TraceId("trace-root".into()),
        => {
            println!("\n=== Inside with_scope! ===");
            println!("trace_id  = {:?}", get_context::<TraceId>("trace_id"));
            println!("span_id   = {:?}", get_context::<SpanId>("span_id"));
            println!("tenant_id = {:?}", get_context::<TenantId>("tenant_id")); // inherited

            // Nested with_scope!
            with_scope! {
                "span_id" => SpanId("span-db-query".into()),
                => {
                    println!("\n=== Nested with_scope! ===");
                    println!("span_id = {:?}", get_context::<SpanId>("span_id"));
                }
            }

            println!("\n=== After nested scope ===");
            println!("span_id = {:?}", get_context::<SpanId>("span_id")); // reverted
        }
    }

    println!("\n=== After outer scope ===");
    println!("span_id = {:?}", get_context::<SpanId>("span_id")); // reverted to default
}
