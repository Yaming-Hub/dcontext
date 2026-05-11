//! # Sample 8: Macros — register_contexts! and manual scoped guards
//!
//! Demonstrates bulk registration with `register_contexts!` and the
//! replacement for the removed `with_scope!` macro.
//!
//! Usage: `cargo run --bin macros`

use dcontext::{sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

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

    sync_ctx::set_context("trace_id", TraceId("trace-root".into()));
    sync_ctx::set_context("tenant_id", TenantId("acme-corp".into()));

    println!("=== Before scoped block ===");
    println!(
        "trace_id  = {:?}",
        sync_ctx::get_context::<TraceId>("trace_id").unwrap()
    );
    println!(
        "span_id   = {:?}",
        sync_ctx::get_context::<SpanId>("span_id").unwrap_or_default()
    );
    println!(
        "tenant_id = {:?}",
        sync_ctx::get_context::<TenantId>("tenant_id").unwrap()
    );

    {
        let _guard = sync_ctx::enter_scope();
        sync_ctx::set_context("span_id", SpanId("span-handler".into()));
        sync_ctx::set_context("trace_id", TraceId("trace-root".into()));
        println!("\n=== Inside scoped block ===");
        println!(
            "trace_id  = {:?}",
            sync_ctx::get_context::<TraceId>("trace_id").unwrap()
        );
        println!(
            "span_id   = {:?}",
            sync_ctx::get_context::<SpanId>("span_id").unwrap()
        );
        println!(
            "tenant_id = {:?}",
            sync_ctx::get_context::<TenantId>("tenant_id").unwrap()
        ); // inherited

        {
            let _guard = sync_ctx::enter_scope();
            sync_ctx::set_context("span_id", SpanId("span-db-query".into()));
            println!("\n=== Nested scoped block ===");
            println!(
                "span_id = {:?}",
                sync_ctx::get_context::<SpanId>("span_id").unwrap()
            );
        }

        println!("\n=== After nested scope ===");
        println!(
            "span_id = {:?}",
            sync_ctx::get_context::<SpanId>("span_id").unwrap()
        ); // reverted
    }

    println!("\n=== After outer scope ===");
    println!(
        "span_id = {:?}",
        sync_ctx::get_context::<SpanId>("span_id").unwrap_or_default()
    ); // reverted to default
}
