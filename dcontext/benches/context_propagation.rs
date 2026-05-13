//! Benchmarks comparing context propagation strategies under concurrency.
//!
//! Run with: cargo bench -p dcontext

use std::sync::Once;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use dcontext::{ContextFutureExt, RegistryBuilder};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct BenchValue(String);

static INIT: Once = Once::new();

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

fn in_scope<R>(f: impl FnOnce() -> R) -> R {
    dcontext::clear();
    let _scope = dcontext::push_scope("bench");
    f()
}

fn populate_context(n: usize) {
    for i in 0..n.min(KEYS.len()) {
        dcontext::set_context_variable(KEYS[i], BenchValue(format!("value-{i}")));
    }
}

fn bench_snapshot_vs_fork(c: &mut Criterion) {
    ensure_registered();

    let mut group = c.benchmark_group("capture_cost");
    for num_keys in [1, 5, 10] {
        group.bench_with_input(
            BenchmarkId::new("snapshot", num_keys),
            &num_keys,
            |b, &n| {
                in_scope(|| {
                    populate_context(n);
                    b.iter(|| {
                        let _snap = dcontext::capture();
                    });
                });
            },
        );

        group.bench_with_input(BenchmarkId::new("fork", num_keys), &num_keys, |b, &n| {
            in_scope(|| {
                populate_context(n);
                b.iter(|| {
                    let _store = dcontext::fork();
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
        let snap = in_scope(|| {
            populate_context(num_keys);
            dcontext::capture()
        });

        group.bench_with_input(
            BenchmarkId::new("snapshot_attach", num_keys),
            &num_keys,
            |b, _| {
                b.iter(|| {
                    in_scope(|| {
                        let _guard = dcontext::attach_snapshot(snap.clone());
                    });
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("fork_with_context", num_keys),
            &num_keys,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    let store = in_scope(|| {
                        populate_context(num_keys);
                        dcontext::fork()
                    });
                    async {}.with(store).await;
                });
            },
        );
    }

    group.finish();
}

fn bench_concurrent_spawn(c: &mut Criterion) {
    ensure_registered();

    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("concurrent_spawn");
    group.sample_size(20);

    for num_tasks in [10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("snapshot", num_tasks),
            &num_tasks,
            |b, &n| {
                b.to_async(&rt).iter(|| async move {
                    let snap = in_scope(|| {
                        populate_context(5);
                        dcontext::capture()
                    });

                    let mut handles = Vec::with_capacity(n);
                    for _ in 0..n {
                        let s = snap.clone();
                        handles.push(tokio::spawn(
                            async move {
                                let _v: BenchValue =
                                    dcontext::get_context_variable(KEYS[0]).unwrap();
                                let _v: BenchValue =
                                    dcontext::get_context_variable(KEYS[2]).unwrap();
                                let _v: BenchValue =
                                    dcontext::get_context_variable(KEYS[4]).unwrap();
                            }
                            .attach(s),
                        ));
                    }
                    for h in handles {
                        let _ = h.await;
                    }
                });
            },
        );

        group.bench_with_input(BenchmarkId::new("fork", num_tasks), &num_tasks, |b, &n| {
            b.to_async(&rt).iter(|| async move {
                let stores = in_scope(|| {
                    populate_context(5);
                    (0..n).map(|_| dcontext::fork()).collect::<Vec<_>>()
                });

                let mut handles = Vec::with_capacity(n);
                for s in stores {
                    handles.push(tokio::spawn(
                        async move {
                            let _v: BenchValue = dcontext::get_context_variable(KEYS[0]).unwrap();
                            let _v: BenchValue = dcontext::get_context_variable(KEYS[2]).unwrap();
                            let _v: BenchValue = dcontext::get_context_variable(KEYS[4]).unwrap();
                        }
                        .with(s),
                    ));
                }
                for h in handles {
                    let _ = h.await;
                }
            });
        });
    }

    group.finish();
}

fn bench_read_throughput(c: &mut Criterion) {
    ensure_registered();

    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("read_throughput");
    group.sample_size(20);

    group.bench_function("fork_100_tasks_10_reads_each", |b| {
        b.to_async(&rt).iter(|| async {
            let stores = in_scope(|| {
                populate_context(10);
                (0..100).map(|_| dcontext::fork()).collect::<Vec<_>>()
            });

            let mut tasks = Vec::with_capacity(100);
            for s in stores {
                tasks.push(tokio::spawn(
                    async move {
                        for i in 0..10 {
                            let _v: BenchValue = dcontext::get_context_variable(KEYS[i]).unwrap();
                        }
                    }
                    .with(s),
                ));
            }
            for t in tasks {
                let _ = t.await;
            }
        });
    });

    group.bench_function("snapshot_100_tasks_10_reads_each", |b| {
        b.to_async(&rt).iter(|| async {
            let snap = in_scope(|| {
                populate_context(10);
                dcontext::capture()
            });

            let mut tasks = Vec::with_capacity(100);
            for _ in 0..100 {
                let s = snap.clone();
                tasks.push(tokio::spawn(
                    async move {
                        for i in 0..10 {
                            let _v: BenchValue = dcontext::get_context_variable(KEYS[i]).unwrap();
                        }
                    }
                    .attach(s),
                ));
            }
            for t in tasks {
                let _ = t.await;
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_snapshot_vs_fork,
    bench_attach_vs_with_fork,
    bench_concurrent_spawn,
    bench_read_throughput,
);
criterion_main!(benches);
