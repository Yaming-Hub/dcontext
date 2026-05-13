//! # Sample: Unified Context — Synchronous Usage
//!
//! Demonstrates the unified top-level API for thread-based code.
//!
//! Usage: `cargo run --bin dual_sync_ctx`

use dcontext::{clear, get_context_variable, push_scope, scope_chain, set_context_variable};

fn main() {
    println!("=== Unified Context — Synchronous Usage ===\n");

    clear();

    println!("--- Basic set/get ---");
    set_context_variable("worker_id", "worker-7".to_string());
    set_context_variable("batch_size", 100u32);

    let wid: Option<String> = get_context_variable("worker_id");
    let bs: Option<u32> = get_context_variable("batch_size");
    println!("  worker_id  = {:?}", wid);
    println!("  batch_size = {:?}", bs);

    println!("\n--- Named scopes (RAII guard) ---");
    {
        let _guard = push_scope("process_batch");
        set_context_variable("batch_size", 50u32);

        let bs: Option<u32> = get_context_variable("batch_size");
        println!(
            "  In scope: chain = {:?}, batch_size = {:?}",
            scope_chain(),
            bs
        );

        {
            let _inner = push_scope("validate_item");
            println!("  Nested: chain = {:?}", scope_chain());
        }
        println!("  After inner drop: chain = {:?}", scope_chain());
    }
    let bs: Option<u32> = get_context_variable("batch_size");
    println!(
        "  After outer drop: chain = {:?}, batch_size = {:?}",
        scope_chain(),
        bs
    );

    println!("\n--- Cross-thread ---");
    set_context_variable("parent_value", "hello from main".to_string());

    let handle = std::thread::spawn(|| {
        let val: Option<String> = get_context_variable("parent_value");
        println!(
            "  [child thread] parent_value = {:?} (None — independent store)",
            val
        );

        set_context_variable("child_value", "from child".to_string());
        let cv: Option<String> = get_context_variable("child_value");
        println!("  [child thread] child_value = {:?}", cv);
    });
    handle.join().unwrap();

    let val: Option<String> = get_context_variable("parent_value");
    println!("  [main thread] parent_value still = {:?}", val);

    println!("\n--- Clear ---");
    clear();
    let val: Option<String> = get_context_variable("parent_value");
    println!("  After clear: parent_value = {:?} (gone)", val);

    println!("\nDone!");
}
