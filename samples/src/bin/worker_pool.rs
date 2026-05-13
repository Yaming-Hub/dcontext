//! # Sample 6: Worker Pool with Context
//!
//! Demonstrates propagating context to a pool of worker threads.
//!
//! Usage: `cargo run --bin worker_pool`

use dcontext::{
    attach_snapshot, capture, get_context_variable, initialize, push_scope, set_context_variable,
    ContextSnapshot, RegistryBuilder,
};
use serde::{Deserialize, Serialize};
use std::sync::mpsc;

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TenantId(String);

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    builder.register::<TenantId>("tenant_id");
    initialize(builder);

    let requests = [
        ("req-001", "tenant-alpha"),
        ("req-002", "tenant-beta"),
        ("req-003", "tenant-gamma"),
    ];

    let mut workers = vec![];
    for i in 0..2 {
        let (worker_tx, worker_rx) = mpsc::channel::<(ContextSnapshot, String)>();
        std::thread::spawn(move || {
            while let Ok((snap, task_name)) = worker_rx.recv() {
                let _guard = attach_snapshot(snap);
                let rid = get_context_variable::<RequestId>("request_id").unwrap();
                let tid = get_context_variable::<TenantId>("tenant_id").unwrap();
                println!(
                    "[worker-{}] {} — request_id={:?}, tenant_id={:?}",
                    i, task_name, rid.0, tid.0
                );
            }
        });
        workers.push(worker_tx);
    }

    for (idx, (req_id, tenant)) in requests.iter().enumerate() {
        let _guard = push_scope("dispatch");
        set_context_variable("request_id", RequestId(req_id.to_string()));
        set_context_variable("tenant_id", TenantId(tenant.to_string()));

        let snap = capture();
        let worker_idx = idx % workers.len();
        workers[worker_idx]
            .send((snap, format!("task-{}", idx)))
            .unwrap();
    }

    drop(workers);
    std::thread::sleep(std::time::Duration::from_millis(100));
    println!("\n[main] all requests dispatched");
}
