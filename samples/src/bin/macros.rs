//! # Sample 8: Macros — register_contexts! and scoped guards
//!
//! Demonstrates bulk registration with `register_contexts!` plus `push_scope`.
//!
//! Usage: `cargo run --bin macros`

use dcontext::{get_context_variable, push_scope, set_context_variable, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct SpanId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TenantId(String);

fn main() {
    let mut builder = RegistryBuilder::new();
    dcontext::register_contexts!(builder, {
        "trace_id"  => TraceId,
        "span_id"   => SpanId,
        "tenant_id" => TenantId,
    });
    dcontext::initialize(builder);

    set_context_variable("trace_id", TraceId("trace-root".into()));
    set_context_variable("tenant_id", TenantId("acme-corp".into()));

    println!("=== Before scoped block ===");
    println!(
        "trace_id  = {:?}",
        get_context_variable::<TraceId>("trace_id").unwrap()
    );
    println!(
        "span_id   = {:?}",
        get_context_variable::<SpanId>("span_id").unwrap_or_default()
    );
    println!(
        "tenant_id = {:?}",
        get_context_variable::<TenantId>("tenant_id").unwrap()
    );

    {
        let _guard = push_scope("handler");
        set_context_variable("span_id", SpanId("span-handler".into()));
        set_context_variable("trace_id", TraceId("trace-root".into()));
        println!("\n=== Inside scoped block ===");
        println!(
            "trace_id  = {:?}",
            get_context_variable::<TraceId>("trace_id").unwrap()
        );
        println!(
            "span_id   = {:?}",
            get_context_variable::<SpanId>("span_id").unwrap()
        );
        println!(
            "tenant_id = {:?}",
            get_context_variable::<TenantId>("tenant_id").unwrap()
        );

        {
            let _guard = push_scope("db-query");
            set_context_variable("span_id", SpanId("span-db-query".into()));
            println!("\n=== Nested scoped block ===");
            println!(
                "span_id = {:?}",
                get_context_variable::<SpanId>("span_id").unwrap()
            );
        }

        println!("\n=== After nested scope ===");
        println!(
            "span_id = {:?}",
            get_context_variable::<SpanId>("span_id").unwrap()
        );
    }

    println!("\n=== After outer scope ===");
    println!(
        "span_id = {:?}",
        get_context_variable::<SpanId>("span_id").unwrap_or_default()
    );
}
