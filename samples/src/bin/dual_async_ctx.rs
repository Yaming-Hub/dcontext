//! # Sample: Unified Context — Async Usage
//!
//! Demonstrates the new `ContextFutureExt` pattern for async code.
//!
//! Usage: `cargo run --bin dual_async_ctx`

use dcontext::{clear, get_context_variable, scope_chain, set_context_variable, ContextFutureExt};

#[tokio::main]
async fn main() {
    println!("=== Unified Context — Async Usage ===\n");
    clear();

    async {
        println!("--- Basic set/get ---");
        set_context_variable("request_id", "req-42".to_string());
        set_context_variable("user_id", 1001u64);

        let rid: Option<String> = get_context_variable("request_id");
        let uid: Option<u64> = get_context_variable("user_id");
        println!("  request_id = {:?}", rid);
        println!("  user_id    = {:?}", uid);

        println!("\n--- Scoped context (auto-reverts) ---");
        async {
            set_context_variable("request_id", "req-scoped".to_string());
            let rid: Option<String> = get_context_variable("request_id");
            println!("  Inside scope: request_id = {:?}", rid);
            println!("  Scope chain: {:?}", scope_chain());

            async {
                println!("  Nested scope chain: {:?}", scope_chain());
            }
            .scope("validate")
            .await;
        }
        .scope("handle_request")
        .await;

        let rid: Option<String> = get_context_variable("request_id");
        println!("  After scope: request_id = {:?} (reverted)", rid);
        println!("  Scope chain: {:?} (empty)", scope_chain());

        println!("\n--- Safe across .await (no leak) ---");
        async {
            println!("  Before yield: chain = {:?}", scope_chain());
            tokio::task::yield_now().await;
            println!("  After yield:  chain = {:?}", scope_chain());
        }
        .scope("io_operation")
        .await;
        println!("  After scope:  chain = {:?} (clean)", scope_chain());

        println!("\n--- Propagate to child tasks via fork ---");
        set_context_variable("trace_id", "trace-abc".to_string());
        let handle = tokio::spawn(
            async {
                let tid: Option<String> = get_context_variable("trace_id");
                println!("  [child task] trace_id = {:?}", tid);
                println!("  [child task] chain    = {:?}", scope_chain());
                set_context_variable("trace_id", "trace-child".to_string());
            }
            .scope("parent_handler")
            .fork(),
        );
        handle.await.unwrap();

        let tid: Option<String> = get_context_variable("trace_id");
        println!("  [parent] trace_id still = {:?}", tid);
    }
    .capture()
    .await;

    println!("\n--- Outside wrapped future ---");
    println!("  scope_chain() = {:?} (empty)", scope_chain());
    println!("\nDone!");
}
