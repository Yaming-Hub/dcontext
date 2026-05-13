//! # Sample 3: Async Task Propagation
//!
//! Demonstrates propagating context across Tokio async tasks using
//! `ContextFutureExt`.
//!
//! Usage: `cargo run --bin async_tasks`

use dcontext::{
    get_context_variable, initialize, set_context_variable, ContextFutureExt, RegistryBuilder,
};
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

    set_context_variable("request_id", RequestId("req-async-001".into()));
    set_context_variable("span_id", SpanId(1));

    async {
        println!(
            "[main task] request_id = {:?}",
            get_context_variable::<RequestId>("request_id").unwrap()
        );
        println!(
            "[main task] span_id    = {:?}",
            get_context_variable::<SpanId>("span_id").unwrap()
        );

        let handle = tokio::spawn(
            async {
                println!(
                    "[child task] request_id = {:?}",
                    get_context_variable::<RequestId>("request_id").unwrap()
                );

                async {
                    set_context_variable("span_id", SpanId(2));
                    println!(
                        "[child scope] span_id = {:?}",
                        get_context_variable::<SpanId>("span_id").unwrap()
                    );
                }
                .scope("child-scope")
                .await;

                println!(
                    "[child task] span_id after scope = {:?}",
                    get_context_variable::<SpanId>("span_id").unwrap()
                );
            }
            .fork(),
        );

        handle.await.unwrap();

        println!(
            "[main task] span_id still = {:?}",
            get_context_variable::<SpanId>("span_id").unwrap()
        );
    }
    .capture()
    .await;
}
