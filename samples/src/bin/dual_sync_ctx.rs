//! # Sample: Dual-Context — Sync Context
//!
//! Demonstrates using `dcontext::sync_ctx` for thread-local context management.
//! All operations in `sync_ctx` target the thread-local store exclusively.
//!
//! Key concepts:
//! - `sync_ctx::push_scope` — named scope with RAII guard
//! - `sync_ctx::set_context` / `get_context` — type-safe value access
//! - `sync_ctx::restore` — initialize from a snapshot (bridging)
//! - `sync_ctx::clear` — reset the thread-local context
//!
//! Usage: `cargo run --bin dual_sync_ctx`

use dcontext::sync_ctx;

fn main() {
    println!("=== dcontext::sync_ctx — Thread-Local Context ===\n");

    // sync_ctx always works — thread-local is always available.
    sync_ctx::clear(); // start clean

    // ── Basic set/get ──────────────────────────────────────────
    println!("--- Basic set/get ---");
    sync_ctx::set_context("worker_id", "worker-7".to_string());
    sync_ctx::set_context("batch_size", 100u32);

    let wid: Option<String> = sync_ctx::get_context("worker_id");
    let bs: Option<u32> = sync_ctx::get_context("batch_size");
    println!("  worker_id  = {:?}", wid);
    println!("  batch_size = {:?}", bs);

    // ── Named scopes with RAII guard ──────────────────────────
    println!("\n--- Named scopes (RAII guard) ---");
    {
        let _guard = sync_ctx::push_scope("process_batch");
        sync_ctx::set_context("batch_size", 50u32);

        let chain = sync_ctx::scope_chain();
        let bs: Option<u32> = sync_ctx::get_context("batch_size");
        println!("  In scope: chain = {:?}, batch_size = {:?}", chain, bs);

        {
            let _inner = sync_ctx::push_scope("validate_item");
            let chain = sync_ctx::scope_chain();
            println!("  Nested: chain = {:?}", chain);
        }
        // inner scope reverted
        let chain = sync_ctx::scope_chain();
        println!("  After inner drop: chain = {:?}", chain);
    }
    // outer scope reverted
    let chain = sync_ctx::scope_chain();
    let bs: Option<u32> = sync_ctx::get_context("batch_size");
    println!("  After outer drop: chain = {:?}, batch_size = {:?}", chain, bs);

    // ── Cross-thread with sync_ctx ────────────────────────────
    println!("\n--- Cross-thread ---");
    sync_ctx::set_context("parent_value", "hello from main".to_string());

    let handle = std::thread::spawn(|| {
        // Each thread has its own thread-local — starts empty
        let val: Option<String> = sync_ctx::get_context("parent_value");
        println!("  [child thread] parent_value = {:?} (None — independent store)", val);

        // Set own values
        sync_ctx::set_context("child_value", "from child".to_string());
        let cv: Option<String> = sync_ctx::get_context("child_value");
        println!("  [child thread] child_value = {:?}", cv);
    });
    handle.join().unwrap();

    // Main thread unaffected
    let val: Option<String> = sync_ctx::get_context("parent_value");
    println!("  [main thread] parent_value still = {:?}", val);

    // ── Clear ─────────────────────────────────────────────────
    println!("\n--- Clear ---");
    sync_ctx::clear();
    let val: Option<String> = sync_ctx::get_context("parent_value");
    println!("  After clear: parent_value = {:?} (gone)", val);

    println!("\nDone!");
}
