//! # Sample 6: Worker Pool with Context
//!
//! Demonstrates propagating context to a pool of worker threads.
//! Each worker inherits the context from the dispatcher.
//!
//! Usage: `cargo run --bin worker_pool`

use dcontext::{register, set_context, get_context, scope, snapshot, attach};
use serde::{Serialize, Deserialize};
use std::sync::mpsc;

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TenantId(String);

fn main() {
    register::<RequestId>("request_id");
    register::<TenantId>("tenant_id");

    // Simulate processing multiple requests, each with its own context.
    let requests = vec![
        ("req-001", "tenant-alpha"),
        ("req-002", "tenant-beta"),
        ("req-003", "tenant-gamma"),
    ];

    // Spawn worker threads, each with its own channel.
    let mut workers = vec![];
    for i in 0..2 {
        let (worker_tx, worker_rx) = mpsc::channel::<(dcontext::ContextSnapshot, String)>();
        std::thread::spawn(move || {
            while let Ok((snap, task_name)) = worker_rx.recv() {
                let _guard = attach(snap);
                let rid = get_context::<RequestId>("request_id");
                let tid = get_context::<TenantId>("tenant_id");
                println!(
                    "[worker-{}] {} — request_id={:?}, tenant_id={:?}",
                    i, task_name, rid.0, tid.0
                );
            }
        });
        workers.push(worker_tx);
    }

    // Dispatch requests to workers (round-robin).
    for (idx, (req_id, tenant)) in requests.iter().enumerate() {
        scope(|| {
            set_context("request_id", RequestId(req_id.to_string()));
            set_context("tenant_id", TenantId(tenant.to_string()));

            let snap = snapshot();
            let worker_idx = idx % workers.len();
            workers[worker_idx]
                .send((snap, format!("task-{}", idx)))
                .unwrap();
        });
    }

    // Drop all senders to signal workers to stop.
    drop(workers);

    // Give workers time to process.
    std::thread::sleep(std::time::Duration::from_millis(100));
    println!("\n[main] all requests dispatched");
}
