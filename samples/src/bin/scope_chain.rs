//! # Sample: Scope Chain
//!
//! Demonstrates the scope chain feature with the unified API.
//!
//! Usage: `cargo run --bin scope_chain`

use dcontext::{
    attach_snapshot, capture, get_context_variable, initialize, push_scope, scope_chain,
    set_context_variable, set_max_scope_chain_len, ContextFutureExt, ContextSnapshot,
    RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[tokio::main]
async fn main() {
    println!("=== 1. Register context type ===");
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    initialize(builder);

    set_context_variable("request_id", RequestId("req-42".into()));
    println!("Registered RequestId; scope_chain = {:?}", scope_chain());

    println!("\n=== 2. Named scopes with push_scope() ===");
    {
        let _g1 = push_scope("gateway");
        println!("After entering 'gateway':  {:?}", scope_chain());
        {
            let _g2 = push_scope("auth");
            println!("After entering 'auth':     {:?}", scope_chain());
            {
                let _g3 = push_scope("handler");
                println!("After entering 'handler':  {:?}", scope_chain());
            }
            println!("After leaving 'handler':   {:?}", scope_chain());
        }
        println!("After leaving 'auth':      {:?}", scope_chain());
    }
    println!("After leaving 'gateway':   {:?}", scope_chain());

    println!("\n=== 3. Every scope is now explicitly named ===");
    {
        let _named = push_scope("visible");
        println!("Named scope 'visible': {:?}", scope_chain());
        {
            let _anonymous = push_scope("anonymous");
            set_context_variable("request_id", RequestId("req-99".into()));
            println!("Inside 'anonymous':     {:?}", scope_chain());
            println!(
                "  request_id = {:?}",
                get_context_variable::<RequestId>("request_id").unwrap()
            );
        }
        println!("After anonymous exits:  {:?}", scope_chain());
    }

    println!("\n=== 4. Cross-process propagation ===");
    let wire_bytes = {
        let _g1 = push_scope("service-a");
        let _g2 = push_scope("endpoint-x");
        set_context_variable("request_id", RequestId("req-cross".into()));
        println!("Sender chain: {:?}", scope_chain());
        let bytes = capture().serialize().expect("serialize failed");
        println!("Serialized to {} bytes", bytes.len());
        bytes
    };

    println!("\n--- Receiver side ---");
    let remote = ContextSnapshot::deserialize(&wire_bytes).expect("deserialize failed");
    {
        let _guard = attach_snapshot(remote.clone());
        println!("After deserialize, chain: {:?}", scope_chain());
        println!(
            "  request_id = {:?}",
            get_context_variable::<RequestId>("request_id").unwrap()
        );
    }

    println!("\n=== 5. Remote prefix + local named scopes ===");
    {
        let _guard = attach_snapshot(remote.clone());
        println!("Remote chain:          {:?}", scope_chain());

        let _g1 = push_scope("service-b");
        println!("+ local 'service-b':   {:?}", scope_chain());

        let _g2 = push_scope("handler-y");
        println!("+ local 'handler-y':   {:?}", scope_chain());
    }

    println!("\n=== 6. set_max_scope_chain_len() ===");
    set_max_scope_chain_len(3);
    println!("Max chain length set to 3");
    {
        let _g1 = push_scope("a");
        let _g2 = push_scope("b");
        let _g3 = push_scope("c");
        println!("After a/b/c:    {:?}", scope_chain());
        {
            let _g4 = push_scope("d");
            println!(
                "After a/b/c/d:  {:?}  (oldest trimmed to stay within limit)",
                scope_chain()
            );
        }
    }
    set_max_scope_chain_len(64);

    println!("\n=== 7. ContextFutureExt::scope() with tokio ===");
    let snap = capture();
    async move {
        async {
            println!("Inside 'async-gateway':      {:?}", scope_chain());

            async {
                println!("Inside 'async-handler':      {:?}", scope_chain());
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                println!("After .await in handler:     {:?}", scope_chain());
            }
            .scope("async-handler")
            .await;

            println!("After 'async-handler' exits: {:?}", scope_chain());
        }
        .scope("async-gateway")
        .await;

        println!("\nDone. Final chain: {:?}", scope_chain());
    }
    .attach(snap)
    .await;
}
