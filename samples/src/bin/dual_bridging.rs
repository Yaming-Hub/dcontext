//! # Sample: Dual-Context — Async-to-Sync Bridging
//!
//! Demonstrates the one-way bridging pattern: capturing a snapshot from
//! `async_ctx` (task-local) and restoring it in `sync_ctx` (thread-local).
//!
//! This is the recommended pattern for:
//! - `spawn_blocking` calls that need request context
//! - CPU-bound work offloaded to a thread pool
//! - FFI calls on a dedicated thread
//!
//! Key concepts:
//! - `async_ctx::snapshot()` — capture current task-local state
//! - `sync_ctx::restore(snapshot)` — initialize thread-local from snapshot
//! - Bridging is one-way: async → sync only (never reverse)
//!
//! Usage: `cargo run --bin dual_bridging`

use dcontext::{async_ctx, sync_ctx, ContextSnapshot};

#[tokio::main]
async fn main() {
    println!("=== Async → Sync Bridging ===\n");

    let initial = ContextSnapshot::empty();
    async_ctx::with_context(initial, async {
        // Set up context in the async handler
        async_ctx::set_context("request_id", "req-bridge-001".to_string());
        async_ctx::set_context("user_id", "alice".to_string());
        let _guard = async_ctx::push_scope("handle_request");

        println!("[async] request_id = {:?}", async_ctx::get_context::<String>("request_id"));
        println!("[async] scope_chain = {:?}", async_ctx::scope_chain());

        // ── Pattern 1: spawn_blocking ─────────────────────────
        println!("\n--- Pattern 1: spawn_blocking ---");
        let snap = async_ctx::snapshot();
        let result = tokio::task::spawn_blocking(move || {
            // Restore the async context into this blocking thread's thread-local
            sync_ctx::restore(snap);

            let rid: Option<String> = sync_ctx::get_context("request_id");
            let uid: Option<String> = sync_ctx::get_context("user_id");
            let chain = sync_ctx::scope_chain();

            println!("  [blocking] request_id = {:?}", rid);
            println!("  [blocking] user_id    = {:?}", uid);
            println!("  [blocking] chain      = {:?}", chain);

            // Can push scopes on the sync side
            let _guard = sync_ctx::push_scope("heavy_computation");
            println!("  [blocking] chain (in scope) = {:?}", sync_ctx::scope_chain());

            // Simulate heavy work
            std::thread::sleep(std::time::Duration::from_millis(10));

            "computation_result"
        }).await.unwrap();
        println!("  [async] got result: {:?}", result);

        // ── Pattern 2: Thread pool / OS threads ───────────────
        println!("\n--- Pattern 2: OS thread with context ---");
        let snap = async_ctx::snapshot();
        let handle = std::thread::spawn(move || {
            sync_ctx::restore(snap);

            let rid: Option<String> = sync_ctx::get_context("request_id");
            let chain = sync_ctx::scope_chain();
            println!("  [OS thread] request_id = {:?}", rid);
            println!("  [OS thread] chain      = {:?}", chain);

            // Modifications on the sync side don't affect the async caller
            sync_ctx::set_context("request_id", "modified-in-thread".to_string());
        });
        handle.join().unwrap();

        // Async context is unaffected
        println!("\n  [async] request_id still = {:?}",
            async_ctx::get_context::<String>("request_id"));

        // ── Pattern 3: Multiple child tasks from same snapshot ─
        println!("\n--- Pattern 3: Fan-out to multiple blocking threads ---");
        let snap = async_ctx::snapshot();

        let mut handles = Vec::new();
        for i in 0..3 {
            let snap_clone = snap.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                sync_ctx::restore(snap_clone);
                let _guard = sync_ctx::push_scope(&format!("worker_{}", i));
                let chain = sync_ctx::scope_chain();
                println!("  [worker {}] chain = {:?}", i, chain);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
    }).await;

    println!("\nDone!");
}
