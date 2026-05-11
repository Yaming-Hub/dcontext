//! # Sample: Dual-Context — Async Context
//!
//! Demonstrates using `dcontext::async_ctx` for task-local context management.
//! All operations in `async_ctx` target the tokio task-local store exclusively.
//!
//! Key concepts:
//! - `async_ctx::with_context` — establishes task-local context for a future
//! - `async_ctx::scope` — scoped context that auto-reverts (safe across .await)
//! - `async_ctx::push_scope` / `scope_chain` — named scope tracking
//! - `async_ctx::set_context` / `get_context` — type-safe value access
//! - `async_ctx::snapshot` — capture for propagation to child tasks
//!
//! Usage: `cargo run --bin dual_async_ctx`

use dcontext::async_ctx;
use dcontext::ContextSnapshot;

#[tokio::main]
async fn main() {
    println!("=== dcontext::async_ctx — Task-Local Context ===\n");

    // All async_ctx operations require a task-local context to be established.
    // Use `with_context` at the entry point (e.g., request handler).
    let initial = ContextSnapshot::empty();

    async_ctx::with_context(initial, async {
        // ── Basic set/get ──────────────────────────────────────────
        println!("--- Basic set/get ---");
        async_ctx::set_context("request_id", "req-42".to_string());
        async_ctx::set_context("user_id", 1001u64);

        let rid: Option<String> = async_ctx::get_context("request_id");
        let uid: Option<u64> = async_ctx::get_context("user_id");
        println!("  request_id = {:?}", rid);
        println!("  user_id    = {:?}", uid);

        // ── Scoped context ────────────────────────────────────────
        println!("\n--- Scoped context (auto-reverts) ---");
        async_ctx::scope("handle_request", async {
            async_ctx::set_context("request_id", "req-scoped".to_string());
            let rid: Option<String> = async_ctx::get_context("request_id");
            println!("  Inside scope: request_id = {:?}", rid);

            let chain = async_ctx::scope_chain();
            println!("  Scope chain: {:?}", chain);

            // Nested scopes
            async_ctx::scope("validate", async {
                let chain = async_ctx::scope_chain();
                println!("  Nested scope chain: {:?}", chain);
            })
            .await;
        })
        .await;

        // After scope exits, values revert
        let rid: Option<String> = async_ctx::get_context("request_id");
        println!("  After scope: request_id = {:?} (reverted)", rid);
        let chain = async_ctx::scope_chain();
        println!("  Scope chain: {:?} (empty)", chain);

        // ── Safe across .await ────────────────────────────────────
        println!("\n--- Safe across .await (no leak) ---");
        async_ctx::scope("io_operation", async {
            println!("  Before yield: chain = {:?}", async_ctx::scope_chain());
            tokio::task::yield_now().await; // simulates I/O
            println!("  After yield:  chain = {:?}", async_ctx::scope_chain());
        })
        .await;
        println!(
            "  After scope:  chain = {:?} (clean)",
            async_ctx::scope_chain()
        );

        // ── Propagate to child tasks ──────────────────────────────
        println!("\n--- Propagate to child tasks via snapshot ---");
        async_ctx::set_context("trace_id", "trace-abc".to_string());
        let _guard = async_ctx::push_scope("parent_handler");

        let child_snap = async_ctx::snapshot();
        let handle = tokio::spawn(async move {
            async_ctx::with_context(child_snap, async {
                let tid: Option<String> = async_ctx::get_context("trace_id");
                let chain = async_ctx::scope_chain();
                println!("  [child task] trace_id = {:?}", tid);
                println!("  [child task] chain    = {:?}", chain);

                // Child modifications are isolated
                async_ctx::set_context("trace_id", "trace-child".to_string());
            })
            .await;
        });
        handle.await.unwrap();

        // Parent is unaffected
        let tid: Option<String> = async_ctx::get_context("trace_id");
        println!("  [parent] trace_id still = {:?}", tid);
    })
    .await;

    // Outside async context, async_ctx gracefully returns defaults
    println!("\n--- Outside async context (graceful no-op) ---");
    let chain = async_ctx::scope_chain();
    println!("  scope_chain() = {:?} (empty — not in task)", chain);

    println!("\nDone!");
}
