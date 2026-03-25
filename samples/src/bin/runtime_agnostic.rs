//! # Sample 11: Runtime-Agnostic Async (ContextFuture)
//!
//! Demonstrates `ContextFuture` — the poll-wrapper that carries context through
//! **any** async executor, not just Tokio. `with_context_future` captures the
//! current thread-local context and installs it on each `poll()`.
//!
//! `ContextFuture` itself has zero Tokio dependency — it works with async-std,
//! smol, or any executor. This sample uses Tokio only for convenience.
//!
//! Usage: `cargo run --bin runtime_agnostic`

use dcontext::{
    register, initialize, set_context, get_context, scope, force_thread_local,
    with_context_future, ContextFuture, snapshot,
};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
struct TraceId(String);

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
struct UserId(String);

/// A regular async function — returns a plain Future, NOT a ContextFuture.
/// Context is still accessible because ContextFuture::poll sets
/// force_thread_local for the entire poll, including nested .await calls.
async fn process_request() -> String {
    let tid: TraceId = get_context("trace_id");
    let uid: UserId = get_context("user_id");
    format!("Processing {} for {}", tid.0, uid.0)
}

/// Another regular async function that calls yet another regular async fn.
async fn handle_request() -> String {
    set_context("trace_id", TraceId("overridden-in-handler".into()));
    let result = process_request().await;
    format!("Handled: {}", result)
}

#[tokio::main]
async fn main() {
    register::<TraceId>("trace_id");
    register::<UserId>("user_id");
    initialize();

    // Set up context in thread-local (force_thread_local because we're
    // inside #[tokio::main] but before any ContextFuture wrapper).
    force_thread_local(|| {
        set_context("trace_id", TraceId("trace-42".into()));
        set_context("user_id", UserId("alice".into()));
    });

    println!("=== ContextFuture: runtime-agnostic async context ===\n");

    // 1. Basic: get/set inside ContextFuture works without force_thread_local.
    //    ContextFuture::poll wraps every poll in force_thread_local automatically.
    println!("1. Basic propagation (no force_thread_local needed inside):");
    let fut = with_context_future(async {
        let tid: TraceId = get_context("trace_id");
        let uid: UserId = get_context("user_id");
        println!("   trace_id = {:?}", tid);
        println!("   user_id  = {:?}", uid);
    });
    fut.await;

    // 2. Calling regular async functions — context propagates through .await.
    println!("\n2. Regular async functions (no ContextFuture wrapping needed inside):");
    let fut = with_context_future(async {
        let result = process_request().await;
        println!("   {}", result);

        // Even nested calls work:
        let result = handle_request().await;
        println!("   {}", result);
    });
    fut.await;

    // 3. Mutations inside ContextFuture don't leak outside.
    println!("\n3. Isolation (mutations don't leak):");
    force_thread_local(|| {
        println!("   Before: trace_id = {:?}", get_context::<TraceId>("trace_id"));
    });

    let fut = with_context_future(async {
        set_context("trace_id", TraceId("modified-inside".into()));
        println!("   Inside:  trace_id = {:?}", get_context::<TraceId>("trace_id"));
    });
    fut.await;

    force_thread_local(|| {
        println!("   After:  trace_id = {:?}", get_context::<TraceId>("trace_id"));
    });

    // 4. Explicit snapshot with ContextFuture::new
    println!("\n4. Explicit snapshot:");
    let snap = force_thread_local(|| {
        scope(|| {
            set_context("trace_id", TraceId("custom-snap".into()));
            snapshot()
        })
    });

    let fut = ContextFuture::new(snap, async {
        println!("   trace_id from snapshot = {:?}", get_context::<TraceId>("trace_id"));
    });
    fut.await;

    // 5. ContextFuture is Send, so it can be spawned on any executor's thread pool.
    println!("\n5. Cross-thread via tokio::spawn:");
    let fut = with_context_future(async {
        let tid: TraceId = get_context("trace_id");
        println!("   Spawned task sees trace_id = {:?} (thread: {:?})",
                 tid, std::thread::current().name());
    });
    tokio::spawn(fut).await.unwrap();

    println!("\nDone! ContextFuture works with any async runtime.");
}
