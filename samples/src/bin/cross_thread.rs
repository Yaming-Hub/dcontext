//! # Sample 2: Cross-Thread Propagation
//!
//! Demonstrates propagating context across threads using:
//! - `spawn_with_context` — library-provided helper
//! - `wrap_with_context` — for passing callbacks to third-party code
//!
//! Usage: `cargo run --bin cross_thread`

use dcontext::{RegistryBuilder, initialize, set_context, get_context, spawn_with_context, wrap_with_context};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceId(String);

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<TraceId>("trace_id");
    initialize(builder);
    set_context("trace_id", TraceId("trace-abc-123".into()));

    // --- spawn_with_context: library-controlled thread ---
    println!("[main] trace_id = {:?}", get_context::<TraceId>("trace_id"));

    let handle = spawn_with_context("worker-1", || {
        let tid = get_context::<TraceId>("trace_id");
        println!("[worker-1] trace_id = {:?}", tid);
        tid.0
    }).expect("spawn failed");

    let result = handle.join().unwrap();
    assert_eq!(result, "trace-abc-123");

    // --- wrap_with_context: third-party callback pattern ---
    // Imagine a library that takes a callback and runs it on its own thread.
    let callback = wrap_with_context(|| {
        let tid = get_context::<TraceId>("trace_id");
        println!("[callback] trace_id = {:?}", tid);
    });

    // Simulate third-party library spawning its own thread.
    let handle = std::thread::spawn(callback);
    handle.join().unwrap();

    println!("[main] done — context never leaked to other threads");
}
