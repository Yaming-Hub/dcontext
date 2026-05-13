//! # Sample 2: Cross-Thread Propagation
//!
//! Demonstrates propagating context across threads using snapshots:
//! - `capture()` — snapshot the current context
//! - `attach_snapshot()` — initialize a worker thread or callback with it
//!
//! Usage: `cargo run --bin cross_thread`

use dcontext::{
    attach_snapshot, capture, get_context_variable, initialize, set_context_variable,
    RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceId(String);

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<TraceId>("trace_id");
    initialize(builder);
    set_context_variable("trace_id", TraceId("trace-abc-123".into()));

    println!(
        "[main] trace_id = {:?}",
        get_context_variable::<TraceId>("trace_id").unwrap()
    );

    let snap = capture();
    let handle = std::thread::Builder::new()
        .name("worker-1".into())
        .spawn(move || {
            let _guard = attach_snapshot(snap);
            let tid = get_context_variable::<TraceId>("trace_id").unwrap();
            println!("[worker-1] trace_id = {:?}", tid);
            tid.0
        })
        .expect("spawn failed");

    let result = handle.join().unwrap();
    assert_eq!(result, "trace-abc-123");

    let callback_snap = capture();
    let callback = move || {
        let _guard = attach_snapshot(callback_snap.clone());
        let tid = get_context_variable::<TraceId>("trace_id").unwrap();
        println!("[callback] trace_id = {:?}", tid);
    };

    let handle = std::thread::spawn(callback);
    handle.join().unwrap();

    println!("[main] done — context never leaked to other threads");
}
