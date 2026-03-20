//! # Sample 3: Async Task Propagation
//!
//! Demonstrates propagating context across Tokio async tasks using:
//! - `with_context` — establishes task-local context for a future
//! - `spawn_with_context_async` — spawns a task with inherited context
//!
//! Usage: `cargo run --bin async_tasks`

use dcontext::{
    register, set_context, get_context, scope, snapshot,
    with_context, spawn_with_context_async, force_thread_local,
};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct SpanId(u64);

#[tokio::main]
async fn main() {
    register::<RequestId>("request_id");
    register::<SpanId>("span_id");

    // Set up initial context (using force_thread_local since we're
    // at the top level of main, before any with_context wrapper).
    let initial_snap = force_thread_local(|| {
        set_context("request_id", RequestId("req-async-001".into()));
        set_context("span_id", SpanId(1));
        snapshot()
    });

    // Wrap the main logic in with_context to establish task-local storage.
    with_context(initial_snap, async {
        println!("[main task] request_id = {:?}", get_context::<RequestId>("request_id"));
        println!("[main task] span_id    = {:?}", get_context::<SpanId>("span_id"));

        // Spawn a child task — context is automatically inherited.
        let handle = spawn_with_context_async(async {
            println!("[child task] request_id = {:?}", get_context::<RequestId>("request_id"));

            // Child task can create its own scopes.
            scope(|| {
                set_context("span_id", SpanId(2));
                println!("[child scope] span_id = {:?}", get_context::<SpanId>("span_id"));
            });

            // After scope exits, span_id reverts.
            println!("[child task] span_id after scope = {:?}", get_context::<SpanId>("span_id"));
        });

        handle.await.unwrap();

        // Main task is unaffected by child's scopes.
        println!("[main task] span_id still = {:?}", get_context::<SpanId>("span_id"));
    })
    .await;
}
