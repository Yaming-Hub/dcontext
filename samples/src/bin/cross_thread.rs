//! # Sample 2: Cross-Thread Propagation
//!
//! Demonstrates propagating context across threads using snapshots:
//! - `sync_ctx::restore` — initialize a worker thread from a snapshot
//! - `sync_ctx::attach` — wrap third-party callbacks with inherited context
//!
//! Usage: `cargo run --bin cross_thread`

use dcontext::{initialize, sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceId(String);

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<TraceId>("trace_id");
    initialize(builder);
    sync_ctx::set_context("trace_id", TraceId("trace-abc-123".into()));

    // --- Snapshot + restore: library-controlled thread ---
    println!(
        "[main] trace_id = {:?}",
        sync_ctx::get_context::<TraceId>("trace_id").unwrap()
    );

    let snap = sync_ctx::snapshot();
    let handle = std::thread::Builder::new()
        .name("worker-1".into())
        .spawn(move || {
            sync_ctx::restore(snap);
            let tid = sync_ctx::get_context::<TraceId>("trace_id").unwrap();
            println!("[worker-1] trace_id = {:?}", tid);
            tid.0
        })
        .expect("spawn failed");

    let result = handle.join().unwrap();
    assert_eq!(result, "trace-abc-123");

    // --- Snapshot + attach: third-party callback pattern ---
    // Imagine a library that takes a callback and runs it on its own thread.
    let callback_snap = sync_ctx::snapshot();
    let callback = move || {
        let _guard = sync_ctx::attach(callback_snap.clone());
        let tid = sync_ctx::get_context::<TraceId>("trace_id").unwrap();
        println!("[worker-1] trace_id = {:?}", tid);
        println!("[callback] trace_id = {:?}", tid);
    };

    // Simulate third-party library spawning its own thread.
    let handle = std::thread::spawn(callback);
    handle.join().unwrap();

    println!("[main] done — context never leaked to other threads");
}
