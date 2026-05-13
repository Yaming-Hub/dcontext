//! # dcontext-dactor: Context Propagation Through Actor Messages
//!
//! This sample keeps the flow conceptual, but now uses the unified dcontext API.
//! It also instantiates `dcontext-dactor` interceptors so the dependency is
//! compiled and available from the samples crate.
//!
//! Run with: `cargo run --bin dactor_propagation`

use dcontext::{
    attach_snapshot, capture, get_context_variable, initialize, push_scope, set_context_variable,
    ContextSnapshot, RegistryBuilder,
};

fn main() {
    println!("dcontext-dactor Sample: Context Propagation Through Actor Messages");
    println!("==================================================================\n");

    let mut builder = RegistryBuilder::new();
    builder.register::<String>("request_id");
    builder.register::<String>("tenant");
    builder.register::<String>("trace_id");
    initialize(builder);

    let _outbound = dcontext_dactor::ContextOutboundInterceptor::default();
    let _inbound = dcontext_dactor::ContextInboundInterceptor::default();

    demo_local_propagation();
    demo_serialization_roundtrip();
    demo_scope_isolation();
}

fn demo_local_propagation() {
    println!("=== Local Propagation (Same Process) ===\n");

    let _scope = push_scope("local-message");
    set_context_variable("request_id", "req-abc-123".to_string());
    set_context_variable("tenant", "acme-corp".to_string());

    let snapshot = capture();
    println!("  Sender captured snapshot with {} values", 2);
    println!(
        "  request_id = {:?}",
        get_context_variable::<String>("request_id").unwrap()
    );
    println!(
        "  tenant     = {:?}",
        get_context_variable::<String>("tenant").unwrap()
    );

    println!("\n  --- message delivered locally ---\n");

    {
        let _attach = attach_snapshot(snapshot);
        let rid: String = get_context_variable("request_id").unwrap();
        let tenant: String = get_context_variable("tenant").unwrap();
        println!("  Receiver sees: request_id = {:?}", rid);
        println!("  Receiver sees: tenant     = {:?}", tenant);
    }

    println!();
}

fn demo_serialization_roundtrip() {
    println!("=== Remote Propagation (Serialized) ===\n");

    let _scope = push_scope("remote-message");
    set_context_variable("request_id", "req-remote-456".to_string());
    set_context_variable("trace_id", "trace-xyz-789".to_string());

    let bytes = capture().serialize().expect("serialization should succeed");
    println!("  Serialized context to {} bytes", bytes.len());
    println!("  --- bytes sent over network ---");

    {
        let snap = ContextSnapshot::deserialize(&bytes).expect("deserialization should succeed");
        let _restored = attach_snapshot(snap);
        let rid: String = get_context_variable("request_id").unwrap();
        let tid: String = get_context_variable("trace_id").unwrap();
        println!("  Remote receiver: request_id = {:?}", rid);
        println!("  Remote receiver: trace_id   = {:?}", tid);
    }

    println!();
}

fn demo_scope_isolation() {
    println!("=== Scope Isolation ===\n");

    let _scope = push_scope("actor-mailbox");
    set_context_variable("request_id", "req-original".to_string());

    let snapshot1 = capture();

    set_context_variable("request_id", "req-different".to_string());
    let snapshot2 = capture();

    {
        let _attach = attach_snapshot(snapshot1);
        let rid: String = get_context_variable("request_id").unwrap();
        println!("  Handler for message 1: request_id = {:?}", rid);
    }

    {
        let _attach = attach_snapshot(snapshot2);
        let rid: String = get_context_variable("request_id").unwrap();
        println!("  Handler for message 2: request_id = {:?}", rid);
    }

    let rid: String = get_context_variable("request_id").unwrap();
    println!("  After both handlers: request_id = {:?} (unchanged)", rid);
    println!();
}
