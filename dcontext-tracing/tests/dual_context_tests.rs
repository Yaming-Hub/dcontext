//! Tests for the dual-context (async_ctx + sync_ctx) redesign.
//!
//! These tests validate that:
//! 1. async_ctx and sync_ctx operate on independent stores
//! 2. The leak bug (depth mismatch) is structurally impossible
//! 3. Bridging (async → sync via snapshot) works correctly
//! 4. AsyncDcontextLayer + scope() don't conflict

use tracing::Instrument;
use tracing_subscriber::prelude::*;

// ══════════════════════════════════════════════════════════════
//  async_ctx basic tests
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn async_ctx_push_pop_scope() {
    // Establish a task-local context
    let snap = dcontext::ContextSnapshot::empty();
    dcontext::async_ctx::with_context(snap, async {
        let chain = dcontext::async_ctx::scope_chain();
        assert!(chain.is_empty());

        let _guard = dcontext::async_ctx::push_scope("test_scope");
        let chain = dcontext::async_ctx::scope_chain();
        assert_eq!(chain, vec!["test_scope"]);
    })
    .await;
}

#[tokio::test]
async fn async_ctx_scope_function() {
    let snap = dcontext::ContextSnapshot::empty();
    dcontext::async_ctx::with_context(snap, async {
        dcontext::async_ctx::scope("outer", async {
            let chain = dcontext::async_ctx::scope_chain();
            assert_eq!(chain, vec!["outer"]);

            dcontext::async_ctx::scope("inner", async {
                let chain = dcontext::async_ctx::scope_chain();
                assert_eq!(chain, vec!["outer", "inner"]);
            })
            .await;

            // inner scope reverted
            let chain = dcontext::async_ctx::scope_chain();
            assert_eq!(chain, vec!["outer"]);
        })
        .await;

        // outer scope reverted
        let chain = dcontext::async_ctx::scope_chain();
        assert!(chain.is_empty());
    })
    .await;
}

#[tokio::test]
async fn async_ctx_set_get_context() {
    let snap = dcontext::ContextSnapshot::empty();
    dcontext::async_ctx::with_context(snap, async {
        dcontext::async_ctx::set_context("test_key", "hello".to_string());
        let val: Option<String> = dcontext::async_ctx::get_context("test_key");
        assert_eq!(val, Some("hello".to_string()));
    })
    .await;
}

#[tokio::test]
async fn async_ctx_snapshot_and_propagate() {
    let snap = dcontext::ContextSnapshot::empty();
    dcontext::async_ctx::with_context(snap, async {
        dcontext::async_ctx::set_context("parent_key", 42u64);
        let _guard = dcontext::async_ctx::push_scope("parent_scope");

        // Take snapshot for child task
        let child_snap = dcontext::async_ctx::snapshot();

        // Spawn child with snapshot
        let handle = tokio::spawn(async move {
            dcontext::async_ctx::with_context(child_snap, async {
                let val: Option<u64> = dcontext::async_ctx::get_context("parent_key");
                assert_eq!(val, Some(42u64));
                // Child sees parent's scope chain
                let chain = dcontext::async_ctx::scope_chain();
                assert_eq!(chain, vec!["parent_scope"]);
            })
            .await;
        });

        handle.await.unwrap();
    })
    .await;
}

// ══════════════════════════════════════════════════════════════
//  sync_ctx basic tests
// ══════════════════════════════════════════════════════════════

#[test]
fn sync_ctx_push_pop_scope() {
    dcontext::sync_ctx::clear();

    let _guard = dcontext::sync_ctx::push_scope("sync_scope");
    let chain = dcontext::sync_ctx::scope_chain();
    assert_eq!(chain, vec!["sync_scope"]);

    drop(_guard);
    let chain = dcontext::sync_ctx::scope_chain();
    assert!(chain.is_empty());
}

#[test]
fn sync_ctx_set_get_context() {
    dcontext::sync_ctx::clear();

    dcontext::sync_ctx::set_context("sync_key", "world".to_string());
    let val: Option<String> = dcontext::sync_ctx::get_context("sync_key");
    assert_eq!(val, Some("world".to_string()));

    dcontext::sync_ctx::clear();
}

#[test]
fn sync_ctx_restore_from_snapshot() {
    dcontext::sync_ctx::clear();

    // Build a snapshot by establishing an async context with a scope
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let snap = rt.block_on(async {
        let empty = dcontext::ContextSnapshot::empty();
        dcontext::async_ctx::with_context(empty, async {
            let _guard = dcontext::async_ctx::push_scope("remote_scope");
            dcontext::async_ctx::snapshot()
        })
        .await
    });

    dcontext::sync_ctx::restore(snap);
    let chain = dcontext::sync_ctx::scope_chain();
    assert_eq!(chain, vec!["remote_scope"]);

    dcontext::sync_ctx::clear();
}

// ══════════════════════════════════════════════════════════════
//  Store independence tests
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn async_and_sync_stores_are_independent() {
    dcontext::sync_ctx::clear();

    let snap = dcontext::ContextSnapshot::empty();
    dcontext::async_ctx::with_context(snap, async {
        // Set value in async context
        dcontext::async_ctx::set_context("shared_name", "async_value".to_string());
        let _guard = dcontext::async_ctx::push_scope("async_scope");

        // Set value in sync context (thread-local)
        dcontext::sync_ctx::set_context("shared_name", "sync_value".to_string());
        let _sync_guard = dcontext::sync_ctx::push_scope("sync_scope");

        // They should be independent
        let async_val: Option<String> = dcontext::async_ctx::get_context("shared_name");
        let sync_val: Option<String> = dcontext::sync_ctx::get_context("shared_name");

        assert_eq!(async_val, Some("async_value".to_string()));
        assert_eq!(sync_val, Some("sync_value".to_string()));

        let async_chain = dcontext::async_ctx::scope_chain();
        let sync_chain = dcontext::sync_ctx::scope_chain();

        assert_eq!(async_chain, vec!["async_scope"]);
        assert_eq!(sync_chain, vec!["sync_scope"]);
    })
    .await;

    dcontext::sync_ctx::clear();
}

// ══════════════════════════════════════════════════════════════
//  Leak prevention test (the key test from the design doc)
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn scope_chain_does_not_leak_on_yield() {
    // Setup: subscriber with AsyncDcontextLayer
    let subscriber =
        tracing_subscriber::registry().with(dcontext_tracing::AsyncDcontextLayer::new());
    let _guard = tracing::subscriber::set_default(subscriber);

    let snap = dcontext::ContextSnapshot::empty();
    dcontext::async_ctx::with_context(snap, async {
        let span = tracing::info_span!("handler");
        async {
            dcontext::async_ctx::scope("inner", async {
                tokio::task::yield_now().await; // forces span exit/re-enter
            })
            .await;
        }
        .instrument(span)
        .await;

        // Task-local is scoped to this task — no leakage possible
        let chain = dcontext::async_ctx::scope_chain();
        assert!(chain.is_empty(), "Leaked scopes: {:?}", chain);
    })
    .await;
}

#[tokio::test]
async fn repeated_yields_do_not_grow_scope_chain() {
    let subscriber =
        tracing_subscriber::registry().with(dcontext_tracing::AsyncDcontextLayer::new());
    let _guard = tracing::subscriber::set_default(subscriber);

    let snap = dcontext::ContextSnapshot::empty();
    dcontext::async_ctx::with_context(snap, async {
        let span = tracing::info_span!("request_handler");

        async {
            for _ in 0..100 {
                dcontext::async_ctx::scope("process_item", async {
                    tokio::task::yield_now().await;
                })
                .await;
            }
        }
        .instrument(span)
        .await;

        // After all iterations, scope chain should be empty
        let chain = dcontext::async_ctx::scope_chain();
        assert!(
            chain.is_empty(),
            "Scope chain leaked after 100 iterations: {:?} (len={})",
            chain,
            chain.len()
        );
    })
    .await;
}

// ══════════════════════════════════════════════════════════════
//  SyncDcontextLayer tests
// ══════════════════════════════════════════════════════════════

#[test]
fn sync_layer_does_not_create_scopes() {
    dcontext::sync_ctx::clear();

    let subscriber =
        tracing_subscriber::registry().with(dcontext_tracing::SyncDcontextLayer::new());
    let _guard = tracing::subscriber::set_default(subscriber);

    {
        let span = tracing::info_span!("sync_handler");
        let _entered = span.enter();

        // Layer should NOT push scopes — scope chain remains empty
        let chain = dcontext::sync_ctx::scope_chain();
        assert!(chain.is_empty(), "layer should not push scopes: {:?}", chain);
    }

    let chain = dcontext::sync_ctx::scope_chain();
    assert!(chain.is_empty(), "Leaked: {:?}", chain);

    dcontext::sync_ctx::clear();
}

// ══════════════════════════════════════════════════════════════
//  Bridging tests (async → sync)
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn bridge_async_to_sync_via_spawn_blocking() {
    let snap = dcontext::ContextSnapshot::empty();
    dcontext::async_ctx::with_context(snap, async {
        dcontext::async_ctx::set_context("request_id", "req-abc".to_string());
        let _guard = dcontext::async_ctx::push_scope("handler");

        let async_snap = dcontext::async_ctx::snapshot();

        let result = tokio::task::spawn_blocking(move || {
            dcontext::sync_ctx::restore(async_snap);

            let val: Option<String> = dcontext::sync_ctx::get_context("request_id");
            let chain = dcontext::sync_ctx::scope_chain();

            (val, chain)
        })
        .await
        .unwrap();

        assert_eq!(result.0, Some("req-abc".to_string()));
        assert_eq!(result.1, vec!["handler"]);
    })
    .await;
}

// ══════════════════════════════════════════════════════════════
//  async_ctx outside of task (no-op behavior)
// ══════════════════════════════════════════════════════════════

#[test]
fn async_ctx_outside_task_returns_empty() {
    // Not in a tokio task — all async_ctx functions should gracefully return defaults
    let chain = dcontext::async_ctx::scope_chain();
    assert!(chain.is_empty());

    let val: Option<String> = dcontext::async_ctx::get_context("any_key");
    assert_eq!(val, None);

    let snap = dcontext::async_ctx::snapshot();
    assert!(snap.scope_chain().is_empty());
}
