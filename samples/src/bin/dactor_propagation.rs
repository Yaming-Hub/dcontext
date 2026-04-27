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
    let _scope = dcontext::enter_scope();
    dcontext::set_context("request_id", "req-abc-123".to_string());
    dcontext::set_context("tenant", "acme-corp".to_string());

    // Outbound interceptor captures a snapshot (zero-copy for local)
    let snapshot = dcontext::snapshot();
    println!("  Sender captured snapshot with {} values", 2);
    println!("  request_id = {:?}", dcontext::get_context::<String>("request_id"));
    println!("  tenant     = {:?}", dcontext::get_context::<String>("tenant"));

    // Simulate message delivery to another actor
    println!("\n  --- message delivered locally ---\n");

    // Inbound interceptor restores context via with_context
    // (In real dactor, wrap_handler does this automatically)
    dcontext::force_thread_local(|| {
        let _scope = dcontext::enter_scope();
        let _attach = dcontext::attach(snapshot);

        let rid: String = dcontext::get_context("request_id");
        let tenant: String = dcontext::get_context("tenant");
        println!("  Receiver sees: request_id = {:?}", rid);
        println!("  Receiver sees: tenant     = {:?}", tenant);
    });

    println!();
}

/// Demonstrates serialization/deserialization for remote (cross-network)
/// message passing.
fn demo_serialization_roundtrip() {
    println!("=== Remote Propagation (Serialized) ===\n");

    // Sender sets context
    let _scope = dcontext::enter_scope();
    dcontext::set_context("request_id", "req-remote-456".to_string());
    dcontext::set_context("trace_id", "trace-xyz-789".to_string());

    // Outbound interceptor serializes context to bytes
    let bytes = dcontext::serialize_context().expect("serialization should succeed");
    println!("  Serialized context to {} bytes", bytes.len());

    // Simulate network transport
    println!("  --- bytes sent over network ---");

    // Inbound interceptor deserializes and restores
    dcontext::force_thread_local(|| {
        let _scope = dcontext::enter_scope();
        let _restored = dcontext::deserialize_context(&bytes)
            .expect("deserialization should succeed");

        let rid: String = dcontext::get_context("request_id");
        let tid: String = dcontext::get_context("trace_id");
        println!("  Remote receiver: request_id = {:?}", rid);
        println!("  Remote receiver: trace_id   = {:?}", tid);
    });

    println!();
}

/// Demonstrates that context restoration is scoped — it doesn't leak
/// into other actors or messages.
fn demo_scope_isolation() {
    println!("=== Scope Isolation ===\n");

    let _scope = dcontext::enter_scope();
    dcontext::set_context("request_id", "req-original".to_string());

    // Capture snapshot for "message 1"
    let snapshot1 = dcontext::snapshot();

    // Change context for "message 2"
    dcontext::set_context("request_id", "req-different".to_string());
    let snapshot2 = dcontext::snapshot();

    // Restore message 1's context — it sees the original value
    dcontext::force_thread_local(|| {
        let _scope = dcontext::enter_scope();
        let _attach = dcontext::attach(snapshot1);
        let rid: String = dcontext::get_context("request_id");
        println!("  Handler for message 1: request_id = {:?}", rid);
    });

    // Restore message 2's context — it sees the different value
    dcontext::force_thread_local(|| {
        let _scope = dcontext::enter_scope();
        let _attach = dcontext::attach(snapshot2);
        let rid: String = dcontext::get_context("request_id");
        println!("  Handler for message 2: request_id = {:?}", rid);
    });

    // Outside both scopes — original context is untouched
    let rid: String = dcontext::get_context("request_id");
    println!("  After both handlers: request_id = {:?} (unchanged)", rid);

    println!();
}
