//! # dcontext-dactor: Context Propagation Through Actor Messages
//!
//! This sample demonstrates how dcontext-dactor interceptors automatically
//! propagate distributed context through actor messages.
//!
//! Since a full dactor runtime requires additional dependencies, this sample
//! shows the interceptor behavior by directly invoking the interceptor methods,
//! demonstrating the outbound capture and inbound restoration flow.
//!
//! Run with: `cargo run --bin dactor_propagation`

/// Simulated example showing how dcontext-dactor interceptors work.
///
/// In a real dactor application, you would register interceptors with the
/// runtime and context flows automatically. This sample demonstrates the
/// conceptual flow step by step.
fn main() {
    println!("dcontext-dactor Sample: Context Propagation Through Actor Messages");
    println!("==================================================================\n");

    // ── Step 1: Register context types ──────────────────────────
    let mut builder = dcontext::RegistryBuilder::new();
    builder.register::<String>("request_id");
    builder.register::<String>("tenant");
    builder.register::<String>("trace_id");
    dcontext::initialize(builder);

    demo_local_propagation();
    demo_serialization_roundtrip();
    demo_scope_isolation();
}

/// Demonstrates how context is captured and restored in local (same-process)
/// message passing. Local propagation preserves ALL context values, including
/// local-only entries.
fn demo_local_propagation() {
    println!("=== Local Propagation (Same Process) ===\n");

    // Sender sets context values
    let _scope = dcontext::sync_ctx::enter_scope();
    dcontext::sync_ctx::set_context("request_id", "req-abc-123".to_string());
    dcontext::sync_ctx::set_context("tenant", "acme-corp".to_string());

    // Outbound interceptor captures a snapshot (zero-copy for local)
    let snapshot = dcontext::sync_ctx::snapshot();
    println!("  Sender captured snapshot with {} values", 2);
    println!(
        "  request_id = {:?}",
        dcontext::sync_ctx::get_context::<String>("request_id").unwrap()
    );
    println!(
        "  tenant     = {:?}",
        dcontext::sync_ctx::get_context::<String>("tenant").unwrap()
    );

    // Simulate message delivery to another actor
    println!("\n  --- message delivered locally ---\n");

    // Inbound interceptor restores context via with_context
    // (In real dactor, wrap_handler does this automatically)
    {
        let _attach = dcontext::sync_ctx::attach(snapshot);

        let rid: String = dcontext::sync_ctx::get_context("request_id").unwrap();
        let tenant: String = dcontext::sync_ctx::get_context("tenant").unwrap();
        println!("  Receiver sees: request_id = {:?}", rid);
        println!("  Receiver sees: tenant     = {:?}", tenant);
    }

    println!();
}

/// Demonstrates serialization/deserialization for remote (cross-network)
/// message passing.
fn demo_serialization_roundtrip() {
    println!("=== Remote Propagation (Serialized) ===\n");

    // Sender sets context
    let _scope = dcontext::sync_ctx::enter_scope();
    dcontext::sync_ctx::set_context("request_id", "req-remote-456".to_string());
    dcontext::sync_ctx::set_context("trace_id", "trace-xyz-789".to_string());

    // Outbound interceptor serializes context to bytes
    let bytes = dcontext::sync_ctx::serialize_context().expect("serialization should succeed");
    println!("  Serialized context to {} bytes", bytes.len());

    // Simulate network transport
    println!("  --- bytes sent over network ---");

    // Inbound interceptor deserializes and restores
    {
        let _restored = dcontext::sync_ctx::deserialize_context(&bytes)
            .expect("deserialization should succeed");

        let rid: String = dcontext::sync_ctx::get_context("request_id").unwrap();
        let tid: String = dcontext::sync_ctx::get_context("trace_id").unwrap();
        println!("  Remote receiver: request_id = {:?}", rid);
        println!("  Remote receiver: trace_id   = {:?}", tid);
    }

    println!();
}

/// Demonstrates that context restoration is scoped — it doesn't leak
/// into other actors or messages.
fn demo_scope_isolation() {
    println!("=== Scope Isolation ===\n");

    let _scope = dcontext::sync_ctx::enter_scope();
    dcontext::sync_ctx::set_context("request_id", "req-original".to_string());

    // Capture snapshot for "message 1"
    let snapshot1 = dcontext::sync_ctx::snapshot();

    // Change context for "message 2"
    dcontext::sync_ctx::set_context("request_id", "req-different".to_string());
    let snapshot2 = dcontext::sync_ctx::snapshot();

    // Restore message 1's context — it sees the original value
    {
        let _attach = dcontext::sync_ctx::attach(snapshot1);
        let rid: String = dcontext::sync_ctx::get_context("request_id").unwrap();
        println!("  Handler for message 1: request_id = {:?}", rid);
    }

    // Restore message 2's context — it sees the different value
    {
        let _attach = dcontext::sync_ctx::attach(snapshot2);
        let rid: String = dcontext::sync_ctx::get_context("request_id").unwrap();
        println!("  Handler for message 2: request_id = {:?}", rid);
    }

    // Outside both scopes — original context is untouched
    let rid: String = dcontext::sync_ctx::get_context("request_id").unwrap();
    println!("  After both handlers: request_id = {:?} (unchanged)", rid);

    println!();
}
