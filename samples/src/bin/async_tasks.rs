//! # Sample 3: Async Task Propagation
//!
//! Demonstrates propagating context across Tokio async tasks using:
//! - `async_ctx::with_context` — establishes task-local context for a future
//! - `async_ctx::snapshot` + `tokio::spawn` — inherits context into child tasks
//!
//! Usage: `cargo run --bin async_tasks`

use dcontext::{async_ctx, initialize, sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct SpanId(u64);

#[tokio::main]
async fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    builder.register::<SpanId>("span_id");
    initialize(builder);

    // Set up initial sync context, then bridge it into task-local storage.
    let initial_snap = {
        sync_ctx::set_context("request_id", RequestId("req-async-001".into()));
        sync_ctx::set_context("span_id", SpanId(1));
        sync_ctx::snapshot()
    };

    // Wrap the main logic in with_context to establish task-local storage.
    async_ctx::with_context(initial_snap, async {
        println!(
            "[main task] request_id = {:?}",
            async_ctx::get_context::<RequestId>("request_id").unwrap()
        );
        println!(
            "[main task] span_id    = {:?}",
            async_ctx::get_context::<SpanId>("span_id").unwrap()
        );

        // Spawn a child task — context is automatically inherited.
        let child_snap = async_ctx::snapshot();
        let handle = tokio::spawn(async_ctx::with_context(child_snap, async {
            println!(
                "[child task] request_id = {:?}",
                async_ctx::get_context::<RequestId>("request_id").unwrap()
            );

            // Child task can create its own async-safe scopes.
            async_ctx::scope("", async {
                async_ctx::set_context("span_id", SpanId(2));
                println!(
                    "[child scope] span_id = {:?}",
                    async_ctx::get_context::<SpanId>("span_id").unwrap()
                );
            })
            .await;

            // After scope exits, span_id reverts.
            println!(
                "[child task] span_id after scope = {:?}",
                async_ctx::get_context::<SpanId>("span_id").unwrap()
            );
        }));

        handle.await.unwrap();

        // Main task is unaffected by child's scopes.
        println!(
            "[main task] span_id still = {:?}",
            async_ctx::get_context::<SpanId>("span_id").unwrap()
        );
    })
    .await;
}
