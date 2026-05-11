//! # Sample: Dual-Context — Tracing Layers
//!
//! Demonstrates using `AsyncDcontextLayer` and `SyncDcontextLayer` for
//! automatic scope management via tracing spans.
//!
//! Key differences from the legacy `DcontextLayer`:
//! - `AsyncDcontextLayer` writes to task-local, persists across yields
//! - `SyncDcontextLayer` writes to thread-local, standard enter/exit
//! - No `force_thread_local` needed — each layer owns its store
//! - No scope leak on yield — structurally impossible
//!
//! Usage: `cargo run --bin dual_tracing_layers`

use dcontext::{async_ctx, sync_ctx, ContextSnapshot};
use dcontext_tracing::{AsyncDcontextLayer, SyncDcontextLayer};
use tracing::Instrument;
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() {
    println!("=== Dual-Context Tracing Layers ===\n");

    // ══════════════════════════════════════════════════════════
    //  AsyncDcontextLayer — for async code
    // ══════════════════════════════════════════════════════════
    println!("--- AsyncDcontextLayer ---");
    println!("  (Scopes persist across yields, no leak)\n");

    {
        // Set up subscriber with AsyncDcontextLayer
        let subscriber = tracing_subscriber::registry()
            .with(AsyncDcontextLayer::new())
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .compact(),
            );
        let _guard = tracing::subscriber::set_default(subscriber);

        let snap = ContextSnapshot::empty();
        async_ctx::with_context(snap, async {
            // Span automatically creates a scope in task-local
            let span = tracing::info_span!("handle_request");
            async {
                let chain = async_ctx::scope_chain();
                println!("  In span: chain = {:?}", chain);

                // Application scopes work alongside
                async_ctx::scope("validate_input", async {
                    let chain = async_ctx::scope_chain();
                    println!("  In app scope: chain = {:?}", chain);

                    // Yield! With AsyncDcontextLayer, this is safe
                    tokio::task::yield_now().await;

                    let chain = async_ctx::scope_chain();
                    println!("  After yield: chain = {:?} (still correct!)", chain);
                })
                .await;

                // After app scope exits
                let chain = async_ctx::scope_chain();
                println!("  After app scope: chain = {:?}", chain);
            }
            .instrument(span)
            .await;

            // After span closes
            let chain = async_ctx::scope_chain();
            println!("  After span close: chain = {:?} (clean)", chain);
        })
        .await;
    }

    // ══════════════════════════════════════════════════════════
    //  Multiple async spans (no leak over iterations)
    // ══════════════════════════════════════════════════════════
    println!("\n--- Multiple iterations (leak-free) ---\n");

    {
        let subscriber = tracing_subscriber::registry().with(AsyncDcontextLayer::new());
        let _guard = tracing::subscriber::set_default(subscriber);

        let snap = ContextSnapshot::empty();
        async_ctx::with_context(snap, async {
            for i in 0..5 {
                let span = tracing::info_span!("process_item", item = i);
                async {
                    async_ctx::scope("inner_work", async {
                        tokio::task::yield_now().await;
                    })
                    .await;
                }
                .instrument(span)
                .await;
            }

            let chain = async_ctx::scope_chain();
            println!(
                "  After 5 iterations: chain = {:?} (empty = no leak)",
                chain
            );
        })
        .await;
    }

    // ══════════════════════════════════════════════════════════
    //  SyncDcontextLayer — for sync code
    // ══════════════════════════════════════════════════════════
    println!("\n--- SyncDcontextLayer ---");
    println!("  (Standard enter/exit on thread-local)\n");

    {
        sync_ctx::clear();

        let subscriber = tracing_subscriber::registry()
            .with(SyncDcontextLayer::new())
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .compact(),
            );
        let _guard = tracing::subscriber::set_default(subscriber);

        {
            let span = tracing::info_span!("sync_handler");
            let _entered = span.enter();

            let chain = sync_ctx::scope_chain();
            println!("  In span: chain = {:?}", chain);

            // Nested span
            {
                let inner = tracing::info_span!("validate");
                let _entered = inner.enter();

                let chain = sync_ctx::scope_chain();
                println!("  Nested: chain = {:?}", chain);
            }

            let chain = sync_ctx::scope_chain();
            println!("  After nested: chain = {:?}", chain);
        }

        let chain = sync_ctx::scope_chain();
        println!("  After all spans: chain = {:?} (clean)", chain);

        sync_ctx::clear();
    }

    // ══════════════════════════════════════════════════════════
    //  Combined: AsyncDcontextLayer + bridging to sync
    // ══════════════════════════════════════════════════════════
    println!("\n--- Combined: async tracing + sync bridging ---\n");

    {
        let subscriber = tracing_subscriber::registry().with(AsyncDcontextLayer::new());
        let _guard = tracing::subscriber::set_default(subscriber);

        let snap = ContextSnapshot::empty();
        async_ctx::with_context(snap, async {
            let span = tracing::info_span!("api_handler");
            async {
                async_ctx::set_context("correlation_id", "corr-123".to_string());

                // Bridge to a blocking thread
                let snap = async_ctx::snapshot();
                let result = tokio::task::spawn_blocking(move || {
                    sync_ctx::restore(snap);
                    let cid: Option<String> = sync_ctx::get_context("correlation_id");
                    let chain = sync_ctx::scope_chain();
                    println!("  [blocking] correlation_id = {:?}", cid);
                    println!("  [blocking] chain = {:?} (from async snapshot)", chain);
                    "ok"
                })
                .await
                .unwrap();
                println!("  [async] blocking result = {:?}", result);
            }
            .instrument(span)
            .await;
        })
        .await;
    }

    println!("\nDone!");
}
