//! # Sample: Scope Chain
//!
//! Demonstrates the scope chain feature: named scopes form a human-readable
//! call chain that propagates across process boundaries via serialization.
//!
//! Usage: `cargo run --bin scope_chain`

use dcontext::{
    RegistryBuilder, initialize, enter_scope, enter_named_scope,
    scope_chain, set_context, get_context, serialize_context,
    deserialize_context, named_scope_async, set_max_scope_chain_len,
    force_thread_local, snapshot, with_context,
};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[tokio::main]
async fn main() {
    // ── 1. Register context types ──────────────────────────────────────
    println!("=== 1. Register context type ===");
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    initialize(builder);

    // Inside tokio we need force_thread_local + with_context for sync APIs.
    // Sections 1-6 use sync scope APIs, so we set up once.
    let snap = force_thread_local(|| {
        set_context("request_id", RequestId("req-42".into()));
        println!("Registered RequestId; scope_chain = {:?}", scope_chain());

        // ── 2. Named scopes appear in the chain ────────────────────────
        println!("\n=== 2. Named scopes with enter_named_scope() ===");
        {
            let _g1 = enter_named_scope("gateway");
            println!("After entering 'gateway':  {:?}", scope_chain());
            {
                let _g2 = enter_named_scope("auth");
                println!("After entering 'auth':     {:?}", scope_chain());
                {
                    let _g3 = enter_named_scope("handler");
                    println!("After entering 'handler':  {:?}", scope_chain());
                }
                println!("After leaving 'handler':   {:?}", scope_chain());
            }
            println!("After leaving 'auth':      {:?}", scope_chain());
        }
        println!("After leaving 'gateway':   {:?}", scope_chain());

        // ── 3. Unnamed scopes are invisible in the chain ───────────────
        println!("\n=== 3. Unnamed scopes are invisible ===");
        {
            let _named = enter_named_scope("visible");
            println!("Named scope 'visible': {:?}", scope_chain());
            {
                let _unnamed = enter_scope();
                set_context("request_id", RequestId("req-99".into()));
                println!("Inside unnamed scope:  {:?}  (chain unchanged)", scope_chain());
                println!("  request_id = {:?}", get_context::<RequestId>("request_id"));
            }
            println!("After unnamed exits:   {:?}", scope_chain());
        }

        // ── 4. Cross-process propagation ───────────────────────────────
        println!("\n=== 4. Cross-process propagation ===");

        // Sender side: build a chain and serialize.
        let wire_bytes = {
            let _g1 = enter_named_scope("service-a");
            let _g2 = enter_named_scope("endpoint-x");
            set_context("request_id", RequestId("req-cross".into()));
            println!("Sender chain: {:?}", scope_chain());
            let bytes = serialize_context().expect("serialize failed");
            println!("Serialized to {} bytes", bytes.len());
            bytes
        };

        // Receiver side: deserialize restores the chain as a remote prefix.
        println!("\n--- Receiver side ---");
        {
            let _scope = enter_scope();
            let _guard = deserialize_context(&wire_bytes).expect("deserialize failed");
            println!("After deserialize, chain: {:?}", scope_chain());
            println!("  request_id = {:?}", get_context::<RequestId>("request_id"));
        }

        // ── 5. Remote prefix + local named scopes ──────────────────────
        println!("\n=== 5. Remote prefix + local named scopes ===");
        {
            let _scope = enter_scope();
            let _guard = deserialize_context(&wire_bytes).expect("deserialize failed");
            println!("Remote chain:          {:?}", scope_chain());

            let _g1 = enter_named_scope("service-b");
            println!("+ local 'service-b':   {:?}", scope_chain());

            let _g2 = enter_named_scope("handler-y");
            println!("+ local 'handler-y':   {:?}", scope_chain());
        }

        // ── 6. set_max_scope_chain_len configuration ───────────────────
        println!("\n=== 6. set_max_scope_chain_len() ===");
        set_max_scope_chain_len(3);
        println!("Max chain length set to 3");
        {
            let _g1 = enter_named_scope("a");
            let _g2 = enter_named_scope("b");
            let _g3 = enter_named_scope("c");
            println!("After a/b/c:    {:?}", scope_chain());
            {
                let _g4 = enter_named_scope("d");
                println!("After a/b/c/d:  {:?}  (oldest trimmed to stay within limit)", scope_chain());
            }
        }
        // Restore default.
        set_max_scope_chain_len(64);

        snapshot()
    });

    // ── 7. named_scope_async with tokio ────────────────────────────────
    println!("\n=== 7. named_scope_async() with tokio ===");
    with_context(snap, async {
        named_scope_async("async-gateway", async {
            println!("Inside 'async-gateway':      {:?}", scope_chain());

            named_scope_async("async-handler", async {
                println!("Inside 'async-handler':      {:?}", scope_chain());

                // Simulate an async I/O operation.
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;

                println!("After .await in handler:     {:?}", scope_chain());
            })
            .await;

            println!("After 'async-handler' exits: {:?}", scope_chain());
        })
        .await;

        println!("\nDone. Final chain: {:?}", scope_chain());
    })
    .await;
}
