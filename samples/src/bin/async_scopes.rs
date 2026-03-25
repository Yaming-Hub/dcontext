//! # Sample 9: Async Scopes with scope_async
//!
//! Demonstrates `scope_async` for creating scoped context changes
//! that work correctly across .await points.
//!
//! Usage: `cargo run --bin async_scopes`

use dcontext::{
    register, initialize, set_context, get_context, scope_async, snapshot,
    with_context, spawn_with_context_async, force_thread_local,
};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct Phase(String);

async fn simulate_io() {
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
}

#[tokio::main]
async fn main() {
    register::<RequestId>("request_id");
    register::<Phase>("phase");
    initialize();

    let snap = force_thread_local(|| {
        set_context("request_id", RequestId("req-async-scoped".into()));
        set_context("phase", Phase("init".into()));
        snapshot()
    });

    with_context(snap, async {
        println!("[main] request_id = {:?}", get_context::<RequestId>("request_id"));
        println!("[main] phase      = {:?}", get_context::<Phase>("phase"));

        // scope_async works across .await points.
        scope_async(async {
            set_context("phase", Phase("processing".into()));
            println!("\n[scope_async] phase = {:?}", get_context::<Phase>("phase"));

            // .await inside scope_async — context is preserved.
            simulate_io().await;

            println!("[scope_async after await] phase = {:?}", get_context::<Phase>("phase"));

            // Spawn a child task from within the async scope.
            let handle = spawn_with_context_async(async {
                println!("[child task] request_id = {:?}", get_context::<RequestId>("request_id"));
                println!("[child task] phase      = {:?}", get_context::<Phase>("phase"));
            });
            handle.await.unwrap();
        })
        .await;

        // After scope_async — phase reverted.
        println!("\n[main after scope_async] phase = {:?}", get_context::<Phase>("phase"));
    })
    .await;
}
