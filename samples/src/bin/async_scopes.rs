//! # Sample 9: Async Scopes with ContextFutureExt::scope
//!
//! Demonstrates scoped context changes that work correctly across `.await`.
//!
//! Usage: `cargo run --bin async_scopes`

use dcontext::{
    get_context_variable, initialize, set_context_variable, ContextFutureExt, RegistryBuilder,
};
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

    set_context_variable("request_id", RequestId("req-async-scoped".into()));
    set_context_variable("phase", Phase("init".into()));

    async {
        println!(
            "[main] request_id = {:?}",
            get_context_variable::<RequestId>("request_id").unwrap()
        );
        println!(
            "[main] phase      = {:?}",
            get_context_variable::<Phase>("phase").unwrap()
        );

        async {
            set_context_variable("phase", Phase("processing".into()));
            println!(
                "\n[async scope] phase = {:?}",
                get_context_variable::<Phase>("phase").unwrap()
            );

            simulate_io().await;

            println!(
                "[async scope after await] phase = {:?}",
                get_context_variable::<Phase>("phase").unwrap()
            );

            let handle = tokio::spawn(
                async {
                    println!(
                        "[child task] request_id = {:?}",
                        get_context_variable::<RequestId>("request_id").unwrap()
                    );
                    println!(
                        "[child task] phase      = {:?}",
                        get_context_variable::<Phase>("phase").unwrap()
                    );
                }
                .fork(),
            );
            handle.await.unwrap();
        }
        .scope("processing")
        .await;

        println!(
            "\n[main after async scope] phase = {:?}",
            get_context_variable::<Phase>("phase").unwrap()
        );
    }
    .capture()
    .await;
}
