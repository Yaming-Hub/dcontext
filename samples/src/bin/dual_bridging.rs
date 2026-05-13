//! # Sample: Unified Context — Async-to-Thread Bridging
//!
//! Demonstrates capturing context in async code and attaching it in blocking
//! work using `attach_snapshot()` or `attach_store()`.
//!
//! Usage: `cargo run --bin dual_bridging`

use dcontext::{
    attach_snapshot, attach_store, capture, fork, get_context_variable, scope_chain,
    set_context_variable, ContextFutureExt,
};

#[tokio::main]
async fn main() {
    println!("=== Async → Thread Bridging ===\n");

    async {
        set_context_variable("request_id", "req-bridge-001".to_string());
        set_context_variable("user_id", "alice".to_string());

        println!(
            "[async] request_id = {:?}",
            get_context_variable::<String>("request_id")
        );

        println!("\n--- Pattern 1: spawn_blocking with snapshot ---");
        let snap = capture();
        let result = tokio::task::spawn_blocking(move || {
            let _guard = attach_snapshot(snap);

            let rid: Option<String> = get_context_variable("request_id");
            let uid: Option<String> = get_context_variable("user_id");
            println!("  [blocking] request_id = {:?}", rid);
            println!("  [blocking] user_id    = {:?}", uid);

            let _guard = dcontext::push_scope("heavy_computation");
            println!("  [blocking] chain      = {:?}", scope_chain());
            std::thread::sleep(std::time::Duration::from_millis(10));
            "computation_result"
        })
        .await
        .unwrap();
        println!("  [async] got result: {:?}", result);

        println!("\n--- Pattern 2: OS thread with ContextStore ---");
        let store = fork();
        let handle = std::thread::spawn(move || {
            let _guard = attach_store(store);
            let rid: Option<String> = get_context_variable("request_id");
            println!("  [OS thread] request_id = {:?}", rid);
            println!("  [OS thread] chain      = {:?}", scope_chain());
            set_context_variable("request_id", "modified-in-thread".to_string());
        });
        handle.join().unwrap();

        println!(
            "\n  [async] request_id still = {:?}",
            get_context_variable::<String>("request_id")
        );

        println!("\n--- Pattern 3: Fan-out to multiple blocking threads ---");
        let snap = capture();
        let mut handles = Vec::new();
        for i in 0..3 {
            let snap_clone = snap.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                let _guard = attach_snapshot(snap_clone);
                let _scope = dcontext::push_scope(&format!("worker_{}", i));
                println!("  [worker {}] chain = {:?}", i, scope_chain());
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
    }
    .scope("handle_request")
    .capture()
    .await;

    println!("\nDone!");
}
