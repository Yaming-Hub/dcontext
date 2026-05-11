//! # Sample: Scope Chain
//!
//! Demonstrates the scope chain feature: named scopes form a human-readable
//! call chain that propagates across process boundaries via serialization.
//!
//! Usage: `cargo run --bin scope_chain`

use dcontext::{async_ctx, initialize, set_max_scope_chain_len, sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[tokio::main]
async fn main() {
    // ── 1. Register context types ──────────────────────────────────────
    println!("=== 1. Register context type ===");
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    initialize(builder);

    // Sections 1-6 use sync scope APIs, so we set up once.
    let snap = {
        sync_ctx::set_context("request_id", RequestId("req-42".into()));
        println!(
            "Registered RequestId; scope_chain = {:?}",
            sync_ctx::scope_chain()
        );

        // ── 2. Named scopes appear in the chain ────────────────────────
        println!("\n=== 2. Named scopes with enter_named_scope() ===");
        {
            let _g1 = sync_ctx::enter_named_scope("gateway");
            println!("After entering 'gateway':  {:?}", sync_ctx::scope_chain());
            {
                let _g2 = sync_ctx::enter_named_scope("auth");
                println!("After entering 'auth':     {:?}", sync_ctx::scope_chain());
                {
                    let _g3 = sync_ctx::enter_named_scope("handler");
                    println!("After entering 'handler':  {:?}", sync_ctx::scope_chain());
                }
                println!("After leaving 'handler':   {:?}", sync_ctx::scope_chain());
            }
            println!("After leaving 'auth':      {:?}", sync_ctx::scope_chain());
        }
        println!("After leaving 'gateway':   {:?}", sync_ctx::scope_chain());

        // ── 3. Unnamed scopes are invisible in the chain ───────────────
        println!("\n=== 3. Unnamed scopes are invisible ===");
        {
            let _named = sync_ctx::enter_named_scope("visible");
            println!("Named scope 'visible': {:?}", sync_ctx::scope_chain());
            {
                let _unnamed = sync_ctx::enter_scope();
                sync_ctx::set_context("request_id", RequestId("req-99".into()));
                println!(
                    "Inside unnamed scope:  {:?}  (chain unchanged)",
                    sync_ctx::scope_chain()
                );
                println!(
                    "  request_id = {:?}",
                    sync_ctx::get_context::<RequestId>("request_id").unwrap()
                );
            }
            println!("After unnamed exits:   {:?}", sync_ctx::scope_chain());
        }

        // ── 4. Cross-process propagation ───────────────────────────────
        println!("\n=== 4. Cross-process propagation ===");

        // Sender side: build a chain and serialize.
        let wire_bytes = {
            let _g1 = sync_ctx::enter_named_scope("service-a");
            let _g2 = sync_ctx::enter_named_scope("endpoint-x");
            sync_ctx::set_context("request_id", RequestId("req-cross".into()));
            println!("Sender chain: {:?}", sync_ctx::scope_chain());
            let bytes = sync_ctx::serialize_context().expect("serialize failed");
            println!("Serialized to {} bytes", bytes.len());
            bytes
        };

        // Receiver side: deserialize restores the chain as a remote prefix.
        println!("\n--- Receiver side ---");
        {
            let _guard = sync_ctx::deserialize_context(&wire_bytes).expect("deserialize failed");
            println!("After deserialize, chain: {:?}", sync_ctx::scope_chain());
            println!(
                "  request_id = {:?}",
                sync_ctx::get_context::<RequestId>("request_id").unwrap()
            );
        }

        // ── 5. Remote prefix + local named scopes ──────────────────────
        println!("\n=== 5. Remote prefix + local named scopes ===");
        {
            let _guard = sync_ctx::deserialize_context(&wire_bytes).expect("deserialize failed");
            println!("Remote chain:          {:?}", sync_ctx::scope_chain());

            let _g1 = sync_ctx::enter_named_scope("service-b");
            println!("+ local 'service-b':   {:?}", sync_ctx::scope_chain());

            let _g2 = sync_ctx::enter_named_scope("handler-y");
            println!("+ local 'handler-y':   {:?}", sync_ctx::scope_chain());
        }

        // ── 6. set_max_scope_chain_len configuration ───────────────────
        println!("\n=== 6. set_max_scope_chain_len() ===");
        set_max_scope_chain_len(3);
        println!("Max chain length set to 3");
        {
            let _g1 = sync_ctx::enter_named_scope("a");
            let _g2 = sync_ctx::enter_named_scope("b");
            let _g3 = sync_ctx::enter_named_scope("c");
            println!("After a/b/c:    {:?}", sync_ctx::scope_chain());
            {
                let _g4 = sync_ctx::enter_named_scope("d");
                println!(
                    "After a/b/c/d:  {:?}  (oldest trimmed to stay within limit)",
                    sync_ctx::scope_chain()
                );
            }
        }
        // Restore default.
        set_max_scope_chain_len(64);

        sync_ctx::snapshot()
    };

    // ── 7. async_ctx::scope with tokio ──────────────────────────────────
    println!("\n=== 7. async_ctx::scope() with tokio ===");
    async_ctx::with_context(snap, async {
        async_ctx::scope("async-gateway", async {
            println!(
                "Inside 'async-gateway':      {:?}",
                async_ctx::scope_chain()
            );

            async_ctx::scope("async-handler", async {
                println!(
                    "Inside 'async-handler':      {:?}",
                    async_ctx::scope_chain()
                );

                // Simulate an async I/O operation.
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;

                println!(
                    "After .await in handler:     {:?}",
                    async_ctx::scope_chain()
                );
            })
            .await;

            println!(
                "After 'async-handler' exits: {:?}",
                async_ctx::scope_chain()
            );
        })
        .await;

        println!("\nDone. Final chain: {:?}", async_ctx::scope_chain());
    })
    .await;
}
