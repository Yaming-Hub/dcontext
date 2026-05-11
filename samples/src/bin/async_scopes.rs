//! # Sample 9: Async Scopes with async_ctx::scope
//!
//! Demonstrates `async_ctx::scope` for creating scoped context changes
//! that work correctly across .await points.
//!
//! Usage: `cargo run --bin async_scopes`

use dcontext::{async_ctx, initialize, sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct Phase(String);

async fn simulate_io() {
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
}

#[tokio::main]
async fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    builder.register::<Phase>("phase");
    initialize(builder);

    let snap = {
        sync_ctx::set_context("request_id", RequestId("req-async-scoped".into()));
        sync_ctx::set_context("phase", Phase("init".into()));
        sync_ctx::snapshot()
    };

    async_ctx::with_context(snap, async {
        println!(
            "[main] request_id = {:?}",
            async_ctx::get_context::<RequestId>("request_id").unwrap()
        );
        println!(
            "[main] phase      = {:?}",
            async_ctx::get_context::<Phase>("phase").unwrap()
        );

        // async_ctx::scope works across .await points.
        async_ctx::scope("", async {
            async_ctx::set_context("phase", Phase("processing".into()));
            println!(
                "\n[async scope] phase = {:?}",
                async_ctx::get_context::<Phase>("phase").unwrap()
            );

            // .await inside the async scope — context is preserved.
            simulate_io().await;

            println!(
                "[async scope after await] phase = {:?}",
                async_ctx::get_context::<Phase>("phase").unwrap()
            );

            // Spawn a child task from within the async scope.
            let child_snap = async_ctx::snapshot();
            let handle = tokio::spawn(async_ctx::with_context(child_snap, async {
                println!(
                    "[child task] request_id = {:?}",
                    async_ctx::get_context::<RequestId>("request_id").unwrap()
                );
                println!(
                    "[child task] phase      = {:?}",
                    async_ctx::get_context::<Phase>("phase").unwrap()
                );
            }));
            handle.await.unwrap();
        })
        .await;

        // After async_ctx::scope — phase reverted.
        println!(
            "\n[main after async scope] phase = {:?}",
            async_ctx::get_context::<Phase>("phase").unwrap()
        );
    })
    .await;
}
