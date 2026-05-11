//! Benchmarks comparing context propagation strategies under concurrency.
//!
//! Run with: cargo bench -p dcontext
//!
//! Measures:
//! - snapshot() + attach() (full clone) vs fork() + with_fork() (Arc sharing)
//! - Sequential vs concurrent task spawning
//! - Varying context sizes (few keys vs many keys)
//! - Read-heavy workloads in spawned tasks

use std::sync::Once;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

use dcontext::{
    attach, enter_scope, force_thread_local, fork, get_context, set_context, snapshot,
    with_context, with_fork, RegistryBuilder,
};

// ── Setup ──────────────────────────────────────────────────────

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct BenchValue(String);

static INIT: Once = Once::new();

// Pre-leaked static keys to avoid Box::leak in hot loops
static KEYS: &[&str] = &[
    "bench_key_0",
    "bench_key_1",
    "bench_key_2",
    "bench_key_3",
    "bench_key_4",
    "bench_key_5",
    "bench_key_6",
    "bench_key_7",
    "bench_key_8",
    "bench_key_9",
];

fn ensure_registered() {
    INIT.call_once(|| {
        let mut builder = RegistryBuilder::new();
        for &key in KEYS {
            builder.register::<BenchValue>(key);
        }
        dcontext::initialize(builder);
    });
}

/// Set N context values in the current scope.
fn populate_context(n: usize) {
    for i in 0..n.min(KEYS.len()) {
        set_context(KEYS[i], BenchValue(format!("value-{}", i)));
    }
}

// ── Micro-benchmarks: single operation cost ────────────────────

fn bench_snapshot_vs_fork(c: &mut Criterion) {
    ensure_registered();

    let mut group = c.benchmark_group("capture_cost");

    for num_keys in [1, 5, 10] {
        group.bench_with_input(
            BenchmarkId::new("snapshot", num_keys),
            &num_keys,
            |b, &n| {
                force_thread_local(|| {
                    let _scope = enter_scope();
                    populate_context(n);
                    b.iter(|| {
                        let _snap = snapshot();
                    });
                });
            },
        );

        group.bench_with_input(BenchmarkId::new("fork", num_keys), &num_keys, |b, &n| {
            force_thread_local(|| {
                let _scope = enter_scope();
                populate_context(n);
                b.iter(|| {
                    let _handle = fork();
                });
            });
        });
    }

    group.finish();
}

fn bench_attach_vs_with_fork(c: &mut Criterion) {
    ensure_registered();

    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("restore_cost");

    for num_keys in [1, 5, 10] {
        let snap = force_thread_local(|| {
            let _scope = enter_scope();
            populate_context(num_keys);
            snapshot()
        });

        let handle = force_thread_local(|| {
            let _scope = enter_scope();
            populate_context(num_keys);
            fork()
        });

        group.bench_with_input(
            BenchmarkId::new("snapshot_attach", num_keys),
            &num_keys,
            |b, _| {
                b.iter(|| {
                    force_thread_local(|| {
                        let _guard = attach(snap.clone());
                    });
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("fork_with_fork", num_keys),
            &num_keys,
            |b, _| {
                let h = handle.clone();
                b.to_async(&rt).iter(|| async {
                    with_fork(h.clone(), async {}).await;
                });
            },
        );
    }

    group.finish();
}

// ── Concurrency benchmarks: spawning N tasks ───────────────────

fn bench_concurrent_spawn(c: &mut Criterion) {
    ensure_registered();

    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("concurrent_spawn");
    group.sample_size(20); // fewer samples for heavy benchmarks

    for num_tasks in [10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("snapshot", num_tasks),
            &num_tasks,
            |b, &n| {
                b.to_async(&rt).iter(|| async {
                    let snap = force_thread_local(|| {
                        let _scope = enter_scope();
                        populate_context(5);
                        snapshot()
                    });

                    let mut handles = Vec::with_capacity(n);
                    for _ in 0..n {
                        let s = snap.clone();
                        handles.push(tokio::spawn(with_context(s, async {
                            // Read 3 values per task
                            let _v: BenchValue = get_context(KEYS[0]);
                            let _v: BenchValue = get_context(KEYS[2]);
                            let _v: BenchValue = get_context(KEYS[4]);
                        })));
                    }
                    for h in handles {
                        let _ = h.await;
                    }
                });
            },
        );

        group.bench_with_input(BenchmarkId::new("fork", num_tasks), &num_tasks, |b, &n| {
            b.to_async(&rt).iter(|| async {
                let handle = force_thread_local(|| {
                    let _scope = enter_scope();
                    populate_context(5);
                    fork()
                });

                let mut handles = Vec::with_capacity(n);
                for _ in 0..n {
                    let h = handle.clone();
                    handles.push(tokio::spawn(with_fork(h, async {
                        let _v: BenchValue = get_context(KEYS[0]);
                        let _v: BenchValue = get_context(KEYS[2]);
                        let _v: BenchValue = get_context(KEYS[4]);
                    })));
                }
                for h in handles {
                    let _ = h.await;
                }
            });
        });
    }

    group.finish();
}

// ── Read throughput under contention ───────────────────────────

fn bench_read_throughput(c: &mut Criterion) {
    ensure_registered();

    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("read_throughput");
    group.sample_size(20);

    group.bench_function("fork_100_tasks_10_reads_each", |b| {
        b.to_async(&rt).iter(|| async {
            let handle = force_thread_local(|| {
                let _scope = enter_scope();
                populate_context(10);
                fork()
            });

            let mut tasks = Vec::with_capacity(100);
            for _ in 0..100 {
                let h = handle.clone();
                tasks.push(tokio::spawn(with_fork(h, async {
                    for i in 0..10 {
                        let _v: BenchValue = get_context(KEYS[i]);
                    }
                })));
            }
            for t in tasks {
                let _ = t.await;
            }
        });
    });

    group.bench_function("snapshot_100_tasks_10_reads_each", |b| {
        b.to_async(&rt).iter(|| async {
            let snap = force_thread_local(|| {
                let _scope = enter_scope();
                populate_context(10);
                snapshot()
            });

            let mut tasks = Vec::with_capacity(100);
            for _ in 0..100 {
                let s = snap.clone();
                tasks.push(tokio::spawn(with_context(s, async {
                    for i in 0..10 {
                        let _v: BenchValue = get_context(KEYS[i]);
                    }
                })));
            }
            for t in tasks {
                let _ = t.await;
            }
        });
    });

    group.finish();
}

// ── Scope depth impact ─────────────────────────────────────────

fn bench_deep_scope(c: &mut Criterion) {
    ensure_registered();

    let mut group = c.benchmark_group("deep_scope_cost");

    for depth in [1, 5, 10] {
        group.bench_with_input(BenchmarkId::new("fork_depth", depth), &depth, |b, &d| {
            b.iter(|| {
                force_thread_local(|| {
                    let mut guards = Vec::new();
                    for _ in 0..d {
                        guards.push(enter_scope());
                    }
                    populate_context(5);
                    let _handle = fork();
                    drop(guards);
                });
            });
        });

        group.bench_with_input(
            BenchmarkId::new("snapshot_depth", depth),
            &depth,
            |b, &d| {
                b.iter(|| {
                    force_thread_local(|| {
                        let mut guards = Vec::new();
                        for _ in 0..d {
                            guards.push(enter_scope());
                        }
                        populate_context(5);
                        let _snap = snapshot();
                        drop(guards);
                    });
                });
            },
        );
    }

    group.finish();
}

// ── High-contention stress tests ───────────────────────────────

fn bench_high_contention(c: &mut Criterion) {
    ensure_registered();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap();

    let mut group = c.benchmark_group("high_contention");
    group.sample_size(10); // heavy benchmarks

    for num_tasks in [200, 500, 1000] {
        group.bench_with_input(
            BenchmarkId::new("snapshot", num_tasks),
            &num_tasks,
            |b, &n| {
                b.to_async(&rt).iter(|| async {
                    let snap = force_thread_local(|| {
                        let _scope = enter_scope();
                        populate_context(10);
                        snapshot()
                    });

                    let mut handles = Vec::with_capacity(n);
                    for _ in 0..n {
                        let s = snap.clone();
                        handles.push(tokio::spawn(with_context(s, async {
                            // Simulate realistic work: read + write + scope
                            let _v: BenchValue = get_context(KEYS[0]);
                            let _v: BenchValue = get_context(KEYS[4]);
                            let _v: BenchValue = get_context(KEYS[9]);
                            // Small CPU work to keep task alive
                            tokio::task::yield_now().await;
                            let _v: BenchValue = get_context(KEYS[2]);
                        })));
                    }
                    for h in handles {
                        let _ = h.await;
                    }
                });
            },
        );

        group.bench_with_input(BenchmarkId::new("fork", num_tasks), &num_tasks, |b, &n| {
            b.to_async(&rt).iter(|| async {
                let handle = force_thread_local(|| {
                    let _scope = enter_scope();
                    populate_context(10);
                    fork()
                });

                let mut handles = Vec::with_capacity(n);
                for _ in 0..n {
                    let h = handle.clone();
                    handles.push(tokio::spawn(with_fork(h, async {
                        let _v: BenchValue = get_context(KEYS[0]);
                        let _v: BenchValue = get_context(KEYS[4]);
                        let _v: BenchValue = get_context(KEYS[9]);
                        tokio::task::yield_now().await;
                        let _v: BenchValue = get_context(KEYS[2]);
                    })));
                }
                for h in handles {
                    let _ = h.await;
                }
            });
        });
    }

    group.finish();
}

// ── Multi-threaded concurrent fork + write (COW pressure) ──────

fn bench_cow_write_pressure(c: &mut Criterion) {
    ensure_registered();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap();

    let mut group = c.benchmark_group("cow_write_pressure");
    group.sample_size(10);

    // Test where child tasks also WRITE (triggers COW in fork, extra alloc in snapshot)
    for num_tasks in [100, 500] {
        group.bench_with_input(
            BenchmarkId::new("snapshot_read_write", num_tasks),
            &num_tasks,
            |b, &n| {
                b.to_async(&rt).iter(|| async {
                    let snap = force_thread_local(|| {
                        let _scope = enter_scope();
                        populate_context(10);
                        snapshot()
                    });

                    let mut handles = Vec::with_capacity(n);
                    for i in 0..n {
                        let s = snap.clone();
                        handles.push(tokio::spawn(with_context(s, async move {
                            let _v: BenchValue = get_context(KEYS[0]);
                            // Write in child — simulates updating a counter/state
                            set_context(KEYS[5], BenchValue(format!("child-{}", i)));
                            let _v: BenchValue = get_context(KEYS[5]);
                        })));
                    }
                    for h in handles {
                        let _ = h.await;
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("fork_read_write", num_tasks),
            &num_tasks,
            |b, &n| {
                b.to_async(&rt).iter(|| async {
                    let handle = force_thread_local(|| {
                        let _scope = enter_scope();
                        populate_context(10);
                        fork()
                    });

                    let mut handles = Vec::with_capacity(n);
                    for i in 0..n {
                        let h = handle.clone();
                        handles.push(tokio::spawn(with_fork(h, async move {
                            let _v: BenchValue = get_context(KEYS[0]);
                            // COW write in forked child
                            set_context(KEYS[5], BenchValue(format!("child-{}", i)));
                            let _v: BenchValue = get_context(KEYS[5]);
                        })));
                    }
                    for h in handles {
                        let _ = h.await;
                    }
                });
            },
        );
    }

    group.finish();
}

// ── Entry point ────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_snapshot_vs_fork,
    bench_attach_vs_with_fork,
    bench_concurrent_spawn,
    bench_read_throughput,
    bench_deep_scope,
    bench_high_contention,
    bench_cow_write_pressure,
);
criterion_main!(benches);
