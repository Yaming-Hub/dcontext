use serde::{Deserialize, Serialize};

// Re-import from the crate root
use crate::*;

// ── Test types ─────────────────────────────────────────────────

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
struct UserId(u64);

// ── Helpers ────────────────────────────────────────────────────

/// Each test needs isolated registration. Since the global registry is shared,
/// we use unique key names per test to avoid conflicts.
fn unique_key(prefix: &str, suffix: &str) -> &'static str {
    // Leak a unique string for each test key — acceptable in tests.
    let s = format!("{}_{}", prefix, suffix);
    Box::leak(s.into_boxed_str())
}

// ══════════════════════════════════════════════════════════════
//  Registration tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_register_and_get_default() {
    let key = unique_key("reg_default", "rid");
    register::<RequestId>(key);
    let val: RequestId = get_context(key);
    assert_eq!(val, RequestId::default());
}

#[test]
fn test_try_register_idempotent() {
    let key = unique_key("reg_idem", "rid");
    try_register::<RequestId>(key).unwrap();
    try_register::<RequestId>(key).unwrap(); // same type = ok
}

#[test]
fn test_try_register_conflict() {
    let key = unique_key("reg_conflict", "val");
    try_register::<RequestId>(key).unwrap();
    let err = try_register::<UserId>(key).unwrap_err();
    assert!(matches!(err, ContextError::AlreadyRegistered(_)));
}

#[test]
#[should_panic]
fn test_get_unregistered_panics() {
    let key = unique_key("unreg_panic", "missing");
    get_context::<RequestId>(key);
}

#[test]
fn test_try_get_unregistered_returns_err() {
    let key = unique_key("unreg_err", "missing");
    let result = try_get_context::<RequestId>(key);
    assert!(matches!(result, Err(ContextError::NotRegistered(_))));
}

// ══════════════════════════════════════════════════════════════
//  Basic get/set tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_set_and_get() {
    let key = unique_key("set_get", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("req-42".into()));
    let val: RequestId = get_context(key);
    assert_eq!(val.0, "req-42");
}

#[test]
fn test_set_unregistered_returns_err() {
    let key = unique_key("set_unreg", "val");
    let err = try_set_context(key, RequestId("x".into())).unwrap_err();
    assert!(matches!(err, ContextError::NotRegistered(_)));
}

#[test]
fn test_type_mismatch_get() {
    let key = unique_key("type_mm_get", "val");
    register::<RequestId>(key);
    set_context(key, RequestId("x".into()));
    let err = try_get_context::<UserId>(key).unwrap_err();
    assert!(matches!(err, ContextError::TypeMismatch(..)));
}

#[test]
fn test_type_mismatch_set() {
    let key = unique_key("type_mm_set", "val");
    register::<RequestId>(key);
    let err = try_set_context(key, UserId(1)).unwrap_err();
    assert!(matches!(err, ContextError::TypeMismatch(..)));
}

// ══════════════════════════════════════════════════════════════
//  Scope tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_scope_shadows_and_reverts() {
    let key = unique_key("scope_shadow", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("parent".into()));

    {
        let _guard = enter_scope();
        set_context(key, RequestId("child".into()));
        assert_eq!(get_context::<RequestId>(key).0, "child");
    }
    // Scope reverted
    assert_eq!(get_context::<RequestId>(key).0, "parent");
}

#[test]
fn test_nested_scopes() {
    let key = unique_key("nested_scope", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("root".into()));

    {
        let _g1 = enter_scope();
        set_context(key, RequestId("level1".into()));

        {
            let _g2 = enter_scope();
            set_context(key, RequestId("level2".into()));
            assert_eq!(get_context::<RequestId>(key).0, "level2");
        }
        assert_eq!(get_context::<RequestId>(key).0, "level1");
    }
    assert_eq!(get_context::<RequestId>(key).0, "root");
}

#[test]
fn test_scope_fn() {
    let key = unique_key("scope_fn", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("before".into()));

    scope(|| {
        set_context(key, RequestId("inside".into()));
        assert_eq!(get_context::<RequestId>(key).0, "inside");
    });

    assert_eq!(get_context::<RequestId>(key).0, "before");
}

#[test]
fn test_scope_inherits_parent() {
    let key = unique_key("scope_inherit", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("parent_val".into()));

    scope(|| {
        // Should see parent value without setting anything
        assert_eq!(get_context::<RequestId>(key).0, "parent_val");
    });
}

#[test]
fn test_scope_partial_override() {
    let key_a = unique_key("scope_partial", "a");
    let key_b = unique_key("scope_partial", "b");
    register::<RequestId>(key_a);
    register::<UserId>(key_b);

    set_context(key_a, RequestId("a_parent".into()));
    set_context(key_b, UserId(10));

    scope(|| {
        // Override only key_a
        set_context(key_a, RequestId("a_child".into()));
        assert_eq!(get_context::<RequestId>(key_a).0, "a_child");
        assert_eq!(get_context::<UserId>(key_b).0, 10); // inherited
    });

    assert_eq!(get_context::<RequestId>(key_a).0, "a_parent");
    assert_eq!(get_context::<UserId>(key_b).0, 10);
}

// ══════════════════════════════════════════════════════════════
//  Snapshot tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_snapshot_captures_current() {
    let key = unique_key("snap_capture", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("snapped".into()));

    let snap = snapshot();

    // Modify after snapshot
    set_context(key, RequestId("modified".into()));

    // Attach snapshot in a new scope
    {
        let _guard = attach(snap);
        assert_eq!(get_context::<RequestId>(key).0, "snapped");
    }
    // Back to modified
    assert_eq!(get_context::<RequestId>(key).0, "modified");
}

#[test]
fn test_snapshot_empty_context() {
    let snap = ContextSnapshot::empty();
    {
        let _guard = attach(snap);
        // No values — should get defaults for registered keys
    }
}

// ══════════════════════════════════════════════════════════════
//  Cross-thread tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_spawn_with_context() {
    let key = unique_key("thread_spawn", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("main-thread".into()));

    let handle = spawn_with_context("test-worker", move || {
        get_context::<RequestId>(key)
    }).unwrap();

    let result = handle.join().unwrap();
    assert_eq!(result.0, "main-thread");
}

#[test]
fn test_wrap_with_context_fn_once() {
    let key = unique_key("wrap_once", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("wrapped".into()));

    let wrapped = wrap_with_context(move || get_context::<RequestId>(key));

    // Change context after wrapping
    set_context(key, RequestId("changed".into()));

    // The wrapped closure should see the snapped value
    let handle = std::thread::spawn(wrapped);
    let result = handle.join().unwrap();
    assert_eq!(result.0, "wrapped");
}

#[test]
fn test_wrap_with_context_fn_multi() {
    let key = unique_key("wrap_multi", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("multi".into()));

    let wrapped = wrap_with_context_fn(move || get_context::<RequestId>(key));

    // Call multiple times
    let r1 = wrapped();
    let r2 = wrapped();
    assert_eq!(r1.0, "multi");
    assert_eq!(r2.0, "multi");
}

// ══════════════════════════════════════════════════════════════
//  Serialization tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_serialize_deserialize_roundtrip() {
    let key = unique_key("serde_rt", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("serialized".into()));

    let bytes = serialize_context().unwrap();

    // Deserialize in a new scope
    {
        let _guard = deserialize_context(&bytes).unwrap();
        assert_eq!(get_context::<RequestId>(key).0, "serialized");
    }
}

#[cfg(feature = "base64")]
#[test]
fn test_serialize_deserialize_string_roundtrip() {
    let key = unique_key("serde_str", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("base64val".into()));

    let encoded = serialize_context_string().unwrap();
    assert!(!encoded.is_empty());

    // Clear and restore
    scope(|| {
        set_context(key, RequestId("cleared".into()));
        {
            let _guard = deserialize_context_string(&encoded).unwrap();
            assert_eq!(get_context::<RequestId>(key).0, "base64val");
        }
    });
}

#[test]
fn test_deserialize_unknown_keys_skipped() {
    let key = unique_key("serde_skip", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("known".into()));

    // Serialize with the current registration
    let bytes = serialize_context().unwrap();

    // The deserialization should work even if there are extra keys —
    // here we just verify it doesn't fail on the known key.
    {
        let _guard = deserialize_context(&bytes).unwrap();
        assert_eq!(get_context::<RequestId>(key).0, "known");
    }
}

#[test]
fn test_serialize_multiple_keys() {
    let key_a = unique_key("serde_multi", "a");
    let key_b = unique_key("serde_multi", "b");
    register::<RequestId>(key_a);
    register::<UserId>(key_b);

    set_context(key_a, RequestId("req-multi".into()));
    set_context(key_b, UserId(42));

    let bytes = serialize_context().unwrap();

    scope(|| {
        let _guard = deserialize_context(&bytes).unwrap();
        assert_eq!(get_context::<RequestId>(key_a).0, "req-multi");
        assert_eq!(get_context::<UserId>(key_b).0, 42);
    });
}

// ══════════════════════════════════════════════════════════════
//  Additional tests (from review feedback S1)
// ══════════════════════════════════════════════════════════════

#[test]
fn test_try_get_registered_but_unset() {
    let key = unique_key("try_get_none", "rid");
    register::<RequestId>(key);
    // Registered but never set — should return Ok(None)
    let result = try_get_context::<RequestId>(key).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_force_thread_local_basic() {
    let key = unique_key("force_tl", "rid");
    register::<RequestId>(key);

    force_thread_local(|| {
        set_context(key, RequestId("forced".into()));
        assert_eq!(get_context::<RequestId>(key).0, "forced");
    });
}

#[test]
fn test_force_thread_local_nesting() {
    let key = unique_key("force_tl_nest", "rid");
    register::<RequestId>(key);

    force_thread_local(|| {
        set_context(key, RequestId("outer".into()));
        force_thread_local(|| {
            // Inner force_thread_local should still work
            assert_eq!(get_context::<RequestId>(key).0, "outer");
            set_context(key, RequestId("inner".into()));
        });
        // After inner returns, force_thread_local should still be active
        assert_eq!(get_context::<RequestId>(key).0, "inner");
    });
}

#[test]
fn test_force_thread_local_panic_safety() {
    let key = unique_key("force_tl_panic", "rid");
    register::<RequestId>(key);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        force_thread_local(|| {
            set_context(key, RequestId("before_panic".into()));
            panic!("intentional panic");
        });
    }));
    assert!(result.is_err());

    // force_thread_local flag should be cleared despite the panic
    // (verified by not hanging/panicking on subsequent calls)
    force_thread_local(|| {
        assert_eq!(get_context::<RequestId>(key).0, "before_panic");
    });
}

// ══════════════════════════════════════════════════════════════
//  ContextKey<T> tests
// ══════════════════════════════════════════════════════════════

#[cfg(feature = "context-key")]
static TEST_CK_KEY: crate::ContextKey<RequestId> = crate::ContextKey::new("test_ck_rid");

#[cfg(feature = "context-key")]
#[test]
fn test_context_key_register_and_get() {
    TEST_CK_KEY.register();
    TEST_CK_KEY.set(RequestId("ck-val".into()));
    assert_eq!(TEST_CK_KEY.get().0, "ck-val");
}

#[cfg(feature = "context-key")]
#[test]
fn test_context_key_try_get_none() {
    let key: crate::ContextKey<UserId> = crate::ContextKey::new(unique_key("ck_none", "uid"));
    key.register();
    assert!(key.try_get().unwrap().is_none());
}

// ══════════════════════════════════════════════════════════════
//  Macro tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_register_contexts_macro() {
    let key_a = unique_key("macro_reg", "a");
    let key_b = unique_key("macro_reg", "b");
    register_contexts! {
        key_a => RequestId,
        key_b => UserId,
    }
    set_context(key_a, RequestId("macro-a".into()));
    set_context(key_b, UserId(77));
    assert_eq!(get_context::<RequestId>(key_a).0, "macro-a");
    assert_eq!(get_context::<UserId>(key_b).0, 77);
}

#[test]
fn test_with_scope_macro() {
    let key = unique_key("macro_scope", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("before".into()));

    with_scope! {
        key => RequestId("inside-macro".into()),
        => {
            assert_eq!(get_context::<RequestId>(key).0, "inside-macro");
        }
    }

    assert_eq!(get_context::<RequestId>(key).0, "before");
}

// ══════════════════════════════════════════════════════════════
//  Config / size limit tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_set_max_context_size_enforced() {
    let key = unique_key("size_limit", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("some-value".into()));

    // Set a very small limit.
    set_max_context_size(5);

    let result = serialize_context();
    assert!(matches!(result, Err(ContextError::ContextTooLarge { .. })));

    // Reset limit.
    set_max_context_size(0);

    // Should succeed now.
    let result = serialize_context();
    assert!(result.is_ok());
}

// ══════════════════════════════════════════════════════════════
//  Async tests (tokio)
// ══════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
mod async_tests {
    use super::*;

    #[tokio::test]
    async fn test_with_context_basic() {
        let key = unique_key("async_basic", "rid");
        register::<RequestId>(key);

        let snap = {
            force_thread_local(|| {
                set_context(key, RequestId("async-val".into()));
                snapshot()
            })
        };

        let result = with_context(snap, async {
            get_context::<RequestId>(key)
        })
        .await;

        assert_eq!(result.0, "async-val");
    }

    #[tokio::test]
    async fn test_scope_async() {
        let key = unique_key("scope_async", "rid");
        register::<RequestId>(key);

        let snap = force_thread_local(|| {
            set_context(key, RequestId("before-async".into()));
            snapshot()
        });

        with_context(snap, async {
            assert_eq!(get_context::<RequestId>(key).0, "before-async");

            scope_async(async {
                set_context(key, RequestId("inside-async".into()));
                assert_eq!(get_context::<RequestId>(key).0, "inside-async");
            })
            .await;

            assert_eq!(get_context::<RequestId>(key).0, "before-async");
        })
        .await;
    }

    #[tokio::test]
    async fn test_spawn_with_context_async() {
        let key = unique_key("async_spawn", "rid");
        register::<RequestId>(key);

        let snap = {
            force_thread_local(|| {
                set_context(key, RequestId("spawned-async".into()));
                snapshot()
            })
        };

        let handle = with_context(snap, async {
            spawn_with_context_async(async {
                get_context::<RequestId>(key)
            })
        })
        .await;

        let result = handle.await.unwrap();
        assert_eq!(result.0, "spawned-async");
    }

    #[tokio::test]
    async fn test_async_scope_isolation() {
        let key = unique_key("async_scope_iso", "rid");
        register::<RequestId>(key);

        let snap = {
            force_thread_local(|| {
                set_context(key, RequestId("outer".into()));
                snapshot()
            })
        };

        with_context(snap, async {
            assert_eq!(get_context::<RequestId>(key).0, "outer");

            scope(|| {
                set_context(key, RequestId("inner".into()));
                assert_eq!(get_context::<RequestId>(key).0, "inner");
            });

            assert_eq!(get_context::<RequestId>(key).0, "outer");
        })
        .await;
    }

    #[tokio::test]
    async fn test_async_serialize_roundtrip() {
        let key = unique_key("async_serde", "rid");
        register::<RequestId>(key);

        let snap = {
            force_thread_local(|| {
                set_context(key, RequestId("async-serde".into()));
                snapshot()
            })
        };

        with_context(snap, async {
            let bytes = serialize_context().unwrap();
            scope(|| {
                set_context(key, RequestId("cleared".into()));
                let _guard = deserialize_context(&bytes).unwrap();
                assert_eq!(get_context::<RequestId>(key).0, "async-serde");
            });
        })
        .await;
    }
}
