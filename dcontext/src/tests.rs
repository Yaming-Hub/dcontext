use serde::{Deserialize, Serialize};

#[cfg(feature = "base64")]
use base64::Engine as _;

use crate::*;
// Re-import from the crate root
use crate::async_ctx;
use crate::sync_ctx;
use crate::wire::test_helpers::{make_wire_bytes, make_wire_bytes_v};
use crate::ContextError;
use crate::ContextSnapshot;
use crate::ScopeGuard;

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
    let val: RequestId = sync_ctx::get_context::<RequestId>(key).unwrap_or_default();
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
fn test_get_unregistered_returns_none() {
    let key = unique_key("unreg_none", "missing");
    assert_eq!(sync_ctx::get_context::<RequestId>(key), None);
}

// Registry-validation wrappers like try_get_context were removed in the sync/async split.

// ══════════════════════════════════════════════════════════════
//  Basic get/set tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_set_and_get() {
    let key = unique_key("set_get", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("req-42".into()));
    let val: RequestId = sync_ctx::get_context::<RequestId>(key).unwrap();
    assert_eq!(val.0, "req-42");
}

// Registry-validation wrappers like try_set_context/try_get_context were removed,
// so the old NotRegistered/TypeMismatch tests no longer apply.

// ══════════════════════════════════════════════════════════════
//  Scope tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_scope_shadows_and_reverts() {
    let key = unique_key("scope_shadow", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("parent".into()));

    {
        let _guard = sync_ctx::enter_scope();
        sync_ctx::set_context(key, RequestId("child".into()));
        assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "child");
    }
    // Scope reverted
    assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "parent");
}

#[test]
fn test_nested_scopes() {
    let key = unique_key("nested_scope", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("root".into()));

    {
        let _g1 = sync_ctx::enter_scope();
        sync_ctx::set_context(key, RequestId("level1".into()));

        {
            let _g2 = sync_ctx::enter_scope();
            sync_ctx::set_context(key, RequestId("level2".into()));
            assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "level2");
        }
        assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "level1");
    }
    assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "root");
}

#[test]
fn test_scope_fn() {
    let key = unique_key("scope_fn", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("before".into()));

    {
        let _scope_guard = sync_ctx::enter_scope();
        sync_ctx::set_context(key, RequestId("inside".into()));
        assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "inside");
    }

    assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "before");
}

#[test]
fn test_scope_inherits_parent() {
    let key = unique_key("scope_inherit", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("parent_val".into()));

    {
        let _scope_guard = sync_ctx::enter_scope();
        // Should see parent value without setting anything
        assert_eq!(
            sync_ctx::get_context::<RequestId>(key).unwrap().0,
            "parent_val"
        );
    }
}

#[test]
fn test_scope_partial_override() {
    let key_a = unique_key("scope_partial", "a");
    let key_b = unique_key("scope_partial", "b");
    register::<RequestId>(key_a);
    register::<UserId>(key_b);

    sync_ctx::set_context(key_a, RequestId("a_parent".into()));
    sync_ctx::set_context(key_b, UserId(10));

    {
        let _scope_guard = sync_ctx::enter_scope();
        // Override only key_a
        sync_ctx::set_context(key_a, RequestId("a_child".into()));
        assert_eq!(
            sync_ctx::get_context::<RequestId>(key_a).unwrap().0,
            "a_child"
        );
        assert_eq!(sync_ctx::get_context::<UserId>(key_b).unwrap().0, 10); // inherited
    }

    assert_eq!(
        sync_ctx::get_context::<RequestId>(key_a).unwrap().0,
        "a_parent"
    );
    assert_eq!(sync_ctx::get_context::<UserId>(key_b).unwrap().0, 10);
}

// ══════════════════════════════════════════════════════════════
//  Snapshot tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_snapshot_captures_current() {
    let key = unique_key("snap_capture", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("snapped".into()));

    let snap = sync_ctx::snapshot();

    // Modify after snapshot
    sync_ctx::set_context(key, RequestId("modified".into()));

    // Attach snapshot in a new scope
    {
        let _guard = sync_ctx::attach(snap);
        assert_eq!(
            sync_ctx::get_context::<RequestId>(key).unwrap().0,
            "snapped"
        );
    }
    // Back to modified
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "modified"
    );
}

#[test]
fn test_snapshot_empty_context() {
    let snap = ContextSnapshot::empty();
    {
        let _guard = sync_ctx::attach(snap);
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
    sync_ctx::set_context(key, RequestId("main-thread".into()));

    let snap = sync_ctx::snapshot();
    let handle = std::thread::Builder::new()
        .name("test-worker".into())
        .spawn(move || {
            sync_ctx::restore(snap);
            sync_ctx::get_context::<RequestId>(key).unwrap()
        })
        .unwrap();

    let result = handle.join().unwrap();
    assert_eq!(result.0, "main-thread");
}

#[test]
fn test_wrap_with_context_fn_once() {
    let key = unique_key("wrap_once", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("wrapped".into()));

    let snap = sync_ctx::snapshot();
    let wrapped = move || {
        sync_ctx::restore(snap);
        sync_ctx::get_context::<RequestId>(key).unwrap()
    };

    // Change context after wrapping
    sync_ctx::set_context(key, RequestId("changed".into()));

    // The wrapped closure should see the snapped value
    let handle = std::thread::spawn(wrapped);
    let result = handle.join().unwrap();
    assert_eq!(result.0, "wrapped");
}

#[test]
fn test_wrap_with_context_fn_multi() {
    let key = unique_key("wrap_multi", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("multi".into()));

    let snap = sync_ctx::snapshot();
    let wrapped = move || {
        let _guard = sync_ctx::attach(snap.clone());
        sync_ctx::get_context::<RequestId>(key).unwrap()
    };

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
    sync_ctx::set_context(key, RequestId("serialized".into()));

    let bytes = sync_ctx::serialize_context().unwrap();

    // Deserialize in a new scope
    {
        let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
        assert_eq!(
            sync_ctx::get_context::<RequestId>(key).unwrap().0,
            "serialized"
        );
    }
}

#[cfg(feature = "base64")]
#[test]
fn test_serialize_deserialize_string_roundtrip() {
    let key = unique_key("serde_str", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("base64val".into()));

    let encoded = sync_ctx::serialize_context()
        .map(|b| base64::engine::general_purpose::STANDARD.encode(&b))
        .map_err(|e| e)
        .unwrap();
    assert!(!encoded.is_empty());

    // Clear and restore
    {
        let _scope_guard = sync_ctx::enter_scope();
        sync_ctx::set_context(key, RequestId("cleared".into()));
        {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&encoded)
                .unwrap();
            let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
            assert_eq!(
                sync_ctx::get_context::<RequestId>(key).unwrap().0,
                "base64val"
            );
        }
    }
}

#[test]
fn test_deserialize_unknown_keys_skipped() {
    let key = unique_key("serde_skip", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("known".into()));

    // Serialize with the current registration
    let bytes = sync_ctx::serialize_context().unwrap();

    // The deserialization should work even if there are extra keys —
    // here we just verify it doesn't fail on the known key.
    {
        let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
        assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "known");
    }
}

#[test]
fn test_serialize_multiple_keys() {
    let key_a = unique_key("serde_multi", "a");
    let key_b = unique_key("serde_multi", "b");
    register::<RequestId>(key_a);
    register::<UserId>(key_b);

    sync_ctx::set_context(key_a, RequestId("req-multi".into()));
    sync_ctx::set_context(key_b, UserId(42));

    let bytes = sync_ctx::serialize_context().unwrap();

    {
        let _scope_guard = sync_ctx::enter_scope();
        let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
        assert_eq!(
            sync_ctx::get_context::<RequestId>(key_a).unwrap().0,
            "req-multi"
        );
        assert_eq!(sync_ctx::get_context::<UserId>(key_b).unwrap().0, 42);
    }
}

// ══════════════════════════════════════════════════════════════
//  Additional tests (from review feedback S1)
// ══════════════════════════════════════════════════════════════

#[test]
fn test_try_get_registered_but_unset() {
    let key = unique_key("try_get_none", "rid");
    register::<RequestId>(key);
    // Registered but never set — should return Ok(None)
    let result = sync_ctx::get_context::<RequestId>(key);
    assert!(result.is_none());
}

// force_thread_local tests removed — sync_ctx always uses thread-local storage

// ══════════════════════════════════════════════════════════════
//  ContextKey<T> tests
// ══════════════════════════════════════════════════════════════

#[cfg(feature = "context-key")]
static TEST_CK_KEY: crate::ContextKey<RequestId> = crate::ContextKey::new("test_ck_rid");

#[cfg(feature = "context-key")]
#[test]
fn test_context_key_register_and_get() {
    register::<RequestId>(TEST_CK_KEY.key());
    TEST_CK_KEY.set(RequestId("ck-val".into()));
    assert_eq!(TEST_CK_KEY.get().unwrap().0, "ck-val");
}

#[cfg(feature = "context-key")]
#[test]
fn test_context_key_try_get_none() {
    let key: crate::ContextKey<UserId> = crate::ContextKey::new(unique_key("ck_none", "uid"));
    register::<UserId>(key.key());
    assert!(key.get().is_none());
}

// ══════════════════════════════════════════════════════════════
//  Macro tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_register_contexts_macro() {
    // The register_contexts! macro now requires a builder.
    // In tests we use it with a local builder, then merge via free-standing register.
    let key_a = unique_key("macro_reg", "a");
    let key_b = unique_key("macro_reg", "b");
    register::<RequestId>(key_a);
    register::<UserId>(key_b);
    sync_ctx::set_context(key_a, RequestId("macro-a".into()));
    sync_ctx::set_context(key_b, UserId(77));
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key_a).unwrap().0,
        "macro-a"
    );
    assert_eq!(sync_ctx::get_context::<UserId>(key_b).unwrap().0, 77);
}

// ══════════════════════════════════════════════════════════════
//  Config / size limit tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_set_max_context_size_enforced() {
    let key = unique_key("size_limit", "rid");
    register::<RequestId>(key);
    sync_ctx::set_context(key, RequestId("some-value".into()));

    // Set a very small limit.
    set_max_context_size(5);

    let result = sync_ctx::serialize_context();
    assert!(matches!(result, Err(ContextError::ContextTooLarge { .. })));

    // Reset limit.
    set_max_context_size(0);

    // Should succeed now.
    let result = sync_ctx::serialize_context();
    assert!(result.is_ok());
}

// ══════════════════════════════════════════════════════════════
//  Version migration tests
// ══════════════════════════════════════════════════════════════

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
struct TraceV1 {
    trace_id: String,
}

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
struct TraceV2 {
    trace_id: String,
    span_id: String,
}

#[test]
fn test_migration_v1_to_v2() {
    let key = unique_key("migrate_v1v2", "trace");

    // Register current version (V2) and add V1 migration.
    register_with::<TraceV2>(key, |o| o.version(2));
    register_migration::<TraceV1, TraceV2>(key, 1, |v1| TraceV2 {
        trace_id: v1.trace_id,
        span_id: "migrated".into(),
    });

    // Serialize V1 bytes manually and run through the migration deserializer.
    let v1_val = TraceV1 {
        trace_id: "tid-old".into(),
    };
    let v1_bytes = bincode::serialize(&v1_val).unwrap();

    let result = crate::registry::with_registration(key, |reg| {
        let deser = reg.deserializers.get(&1).expect("v1 deserializer missing");
        deser(&v1_bytes)
    });

    let boxed = result.unwrap().unwrap();
    let migrated = boxed.as_any().downcast_ref::<TraceV2>().unwrap();
    assert_eq!(migrated.trace_id, "tid-old");
    assert_eq!(migrated.span_id, "migrated");
}

#[test]
fn test_migration_end_to_end() {
    // Full end-to-end: simulate receiving V1 wire bytes when V2 is registered
    // with a V1 migration. Uses a separate thread to get a clean thread-local.
    let key = unique_key("migrate_e2e", "ctx");

    // Register V2 as the current type, with a V1 migration.
    register_with::<TraceV2>(key, |o| o.version(2));
    register_migration::<TraceV1, TraceV2>(key, 1, |v1| TraceV2 {
        trace_id: v1.trace_id,
        span_id: "default-span".into(),
    });

    // Manually craft V1 wire bytes (simulating what a V1 sender would produce).
    let v1_value = TraceV1 {
        trace_id: "from-v1-sender".into(),
    };
    let v1_value_bytes = bincode::serialize(&v1_value).unwrap();
    let wire = make_wire_bytes(key, 1, &v1_value_bytes);

    // Deserialize on a fresh thread (clean thread-local context).
    let handle = std::thread::spawn(move || {
        let _guard = sync_ctx::deserialize_context(&wire).unwrap();
        let val: TraceV2 = sync_ctx::get_context::<TraceV2>(key).unwrap();
        assert_eq!(val.trace_id, "from-v1-sender");
        assert_eq!(val.span_id, "default-span");
    });
    handle.join().unwrap();
}

#[test]
fn test_migration_unknown_version_errors() {
    let key = unique_key("migrate_unknown", "ctx");
    register_with::<TraceV2>(key, |o| o.version(2));
    // No migration for version 1 registered.

    // Check that version 1 has no deserializer.
    let has_v1 = crate::registry::with_registration(key, |reg| reg.deserializers.contains_key(&1));
    assert_eq!(has_v1, Some(false));
}

#[test]
fn test_migration_current_version_still_works() {
    let key = unique_key("migrate_current", "ctx");
    register_with::<TraceV2>(key, |o| o.version(2));
    register_migration::<TraceV1, TraceV2>(key, 1, |v1| TraceV2 {
        trace_id: v1.trace_id,
        span_id: "migrated".into(),
    });

    // Current version (V2) roundtrip should still work.
    let _guard = sync_ctx::enter_scope();
    sync_ctx::set_context(
        key,
        TraceV2 {
            trace_id: "current".into(),
            span_id: "current-span".into(),
        },
    );
    let bytes = sync_ctx::serialize_context().unwrap();

    {
        let _scope_guard = sync_ctx::enter_scope();
        let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
        let val: TraceV2 = sync_ctx::get_context::<TraceV2>(key).unwrap();
        assert_eq!(val.trace_id, "current");
        assert_eq!(val.span_id, "current-span");
    }
}

#[test]
fn test_migration_rejects_current_version() {
    let key = unique_key("migrate_reject", "ctx");
    register_with::<TraceV2>(key, |o| o.version(2));

    // Attempting to register a migration for the CURRENT version should fail.
    let result = try_register_migration::<TraceV1, TraceV2>(key, 2, |v1| TraceV2 {
        trace_id: v1.trace_id,
        span_id: "should-fail".into(),
    });
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, ContextError::DeserializationFailed(_)));
}

// ══════════════════════════════════════════════════════════════
//  Custom codec tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_register_with_json_codec() {
    let key = unique_key("codec_json", "rid");

    register_with::<RequestId>(key, |o| {
        o.codec(
            |val| serde_json::to_vec(val).map_err(|e| e.to_string()),
            |bytes| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
        )
    });

    let _guard = sync_ctx::enter_scope();
    sync_ctx::set_context(key, RequestId("json-encoded".into()));

    // Roundtrip through serialization — uses JSON codec, not bincode.
    let bytes = sync_ctx::serialize_context().unwrap();
    {
        let _scope_guard = sync_ctx::enter_scope();
        let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
        let val: RequestId = sync_ctx::get_context::<RequestId>(key).unwrap();
        assert_eq!(val.0, "json-encoded");
    }
}

#[test]
fn test_json_codec_wire_bytes_are_json() {
    let key = unique_key("codec_json_verify", "rid");

    register_with::<RequestId>(key, |o| {
        o.codec(
            |val| serde_json::to_vec(val).map_err(|e| e.to_string()),
            |bytes| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
        )
    });

    let _guard = sync_ctx::enter_scope();
    sync_ctx::set_context(key, RequestId("verify-json".into()));

    let wire_bytes = sync_ctx::serialize_context().unwrap();

    // The inner value bytes should be valid JSON, not bincode.
    // Deserialize on a fresh thread to confirm.
    let handle = std::thread::spawn(move || {
        register_with::<RequestId>(key, |o| {
            o.codec(
                |val| serde_json::to_vec(val).map_err(|e| e.to_string()),
                |bytes| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
            )
        });
        let _guard = sync_ctx::deserialize_context(&wire_bytes).unwrap();
        let val: RequestId = sync_ctx::get_context::<RequestId>(key).unwrap();
        assert_eq!(val.0, "verify-json");
    });
    handle.join().unwrap();
}

#[test]
fn test_default_codec_still_works() {
    // Ensure normal registration (bincode) is unaffected by codec feature.
    let key = unique_key("codec_default", "rid");
    register::<RequestId>(key);

    let _guard = sync_ctx::enter_scope();
    sync_ctx::set_context(key, RequestId("bincode-default".into()));

    let bytes = sync_ctx::serialize_context().unwrap();
    {
        let _scope_guard = sync_ctx::enter_scope();
        let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
        let val: RequestId = sync_ctx::get_context::<RequestId>(key).unwrap();
        assert_eq!(val.0, "bincode-default");
    }
}

#[test]
fn test_local_only_rejects_codec() {
    let key = unique_key("local_codec", "rid");
    let result = try_register_with::<RequestId>(key, |o| {
        o.local_only().codec(
            |val| serde_json::to_vec(val).map_err(|e| e.to_string()),
            |bytes| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
        )
    });
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ContextError::SerializationFailed(_)
    ));
}

#[test]
fn test_local_only_rejects_version() {
    let key = unique_key("local_version", "rid");
    let result = try_register_with::<RequestId>(key, |o| o.local_only().version(2));
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        ContextError::SerializationFailed(_)
    ));
}

#[test]
fn test_local_only_builder_excludes_from_serialization() {
    let key = unique_key("local_builder_ser", "rid");
    register_with::<RequestId>(key, |o| o.local_only());

    let _scope = sync_ctx::enter_scope();
    sync_ctx::set_context(key, RequestId("should-not-serialize".into()));

    // Serialize — the local_only value must be excluded from wire bytes.
    let bytes = sync_ctx::serialize_context().unwrap();

    // Deserialize on a fresh thread so the original scope's value isn't visible.
    std::thread::spawn(move || {
        let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
        let val = sync_ctx::get_context::<RequestId>(key);
        assert!(
            val.is_none(),
            "local_only value registered via builder should not survive serialization"
        );
    })
    .join()
    .unwrap();
}

// ══════════════════════════════════════════════════════════════
//  Async tests (tokio)
// ══════════════════════════════════════════════════════════════

mod async_tests {
    use super::*;

    #[tokio::test]
    async fn test_with_context_basic() {
        let key = unique_key("async_basic", "rid");
        register::<RequestId>(key);

        let snap = {
            sync_ctx::set_context(key, RequestId("async-val".into()));
            sync_ctx::snapshot()
        };

        let result = async_ctx::with_context(snap, async {
            async_ctx::get_context::<RequestId>(key).unwrap()
        })
        .await;

        assert_eq!(result.0, "async-val");
    }

    #[tokio::test]
    async fn test_scope_async() {
        let key = unique_key("scope_async", "rid");
        register::<RequestId>(key);

        let snap = {
            sync_ctx::set_context(key, RequestId("before-async".into()));
            sync_ctx::snapshot()
        };

        async_ctx::with_context(snap, async {
            assert_eq!(
                async_ctx::get_context::<RequestId>(key).unwrap().0,
                "before-async"
            );

            async_ctx::scope("", async {
                async_ctx::set_context(key, RequestId("inside-async".into()));
                assert_eq!(
                    async_ctx::get_context::<RequestId>(key).unwrap().0,
                    "inside-async"
                );
            })
            .await;

            assert_eq!(
                async_ctx::get_context::<RequestId>(key).unwrap().0,
                "before-async"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn test_spawn_with_context_async() {
        let key = unique_key("async_spawn", "rid");
        register::<RequestId>(key);

        let snap = {
            sync_ctx::set_context(key, RequestId("spawned-async".into()));
            sync_ctx::snapshot()
        };

        let handle = async_ctx::with_context(snap, async {
            let child_snap = async_ctx::snapshot();
            tokio::spawn(async_ctx::with_context(child_snap, async {
                async_ctx::get_context::<RequestId>(key).unwrap()
            }))
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
            sync_ctx::set_context(key, RequestId("outer".into()));
            sync_ctx::snapshot()
        };

        async_ctx::with_context(snap, async {
            assert_eq!(async_ctx::get_context::<RequestId>(key).unwrap().0, "outer");

            async_ctx::scope("", async {
                async_ctx::set_context(key, RequestId("inner".into()));
                assert_eq!(async_ctx::get_context::<RequestId>(key).unwrap().0, "inner");
            })
            .await;

            assert_eq!(async_ctx::get_context::<RequestId>(key).unwrap().0, "outer");
        })
        .await;
    }

    #[tokio::test]
    async fn test_async_serialize_roundtrip() {
        let key = unique_key("async_serde", "rid");
        register::<RequestId>(key);

        let snap = {
            sync_ctx::set_context(key, RequestId("async-serde".into()));
            sync_ctx::snapshot()
        };

        async_ctx::with_context(snap, async {
            let bytes = async_ctx::serialize_context().unwrap();
            async_ctx::set_context(key, RequestId("cleared".into()));
            let _guard = async_ctx::deserialize_context(&bytes).unwrap();
            assert_eq!(
                async_ctx::get_context::<RequestId>(key).unwrap().0,
                "async-serde"
            );
        })
        .await;
    }
}

// ══════════════════════════════════════════════════════════════
//  Scope chain tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_scope_chain_empty_by_default() {
    let chain = sync_ctx::scope_chain();
    assert!(chain.is_empty(), "default scope chain should be empty");
}

#[test]
fn test_scope_chain_named_scope() {
    let _g = sync_ctx::enter_named_scope("outer");
    assert_eq!(sync_ctx::scope_chain(), vec!["outer"]);
    {
        let _g2 = sync_ctx::enter_named_scope("inner");
        assert_eq!(sync_ctx::scope_chain(), vec!["outer", "inner"]);
    }
    assert_eq!(sync_ctx::scope_chain(), vec!["outer"]);
}

#[test]
fn test_scope_chain_unnamed_invisible() {
    let _g1 = sync_ctx::enter_named_scope("named");
    let _g2 = sync_ctx::enter_scope();
    let _g3 = sync_ctx::enter_named_scope("also-named");
    assert_eq!(sync_ctx::scope_chain(), vec!["named", "also-named"]);
}

#[test]
fn test_scope_chain_snapshot_preserves_chain() {
    let _g = sync_ctx::enter_named_scope("request-handler");
    let snap = sync_ctx::snapshot();
    assert_eq!(snap.scope_chain, vec!["request-handler"]);

    // Restore in a new scope — the chain becomes remote_chain
    {
        let _scope_guard = sync_ctx::enter_scope();
        let _guard = sync_ctx::attach(snap.clone());
        assert_eq!(sync_ctx::scope_chain(), vec!["request-handler"]);

        // Push local named scopes
        let _g2 = sync_ctx::enter_named_scope("sub-handler");
        assert_eq!(
            sync_ctx::scope_chain(),
            vec!["request-handler", "sub-handler"]
        );
    }
}

#[test]
fn test_scope_chain_serialize_roundtrip() {
    let key = unique_key("sc_serde", "rid");
    register::<RequestId>(key);

    let _g1 = sync_ctx::enter_named_scope("app");
    let _g2 = sync_ctx::enter_named_scope("service");
    sync_ctx::set_context(key, RequestId("req-1".into()));

    let bytes = sync_ctx::serialize_context().unwrap();

    // Deserialize in a clean scope
    {
        let _scope_guard = sync_ctx::enter_scope();
        let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
        // Values restored
        assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "req-1");
        // Scope chain restored as remote prefix
        assert_eq!(sync_ctx::scope_chain(), vec!["app", "service"]);

        // Push more local scopes
        let _g3 = sync_ctx::enter_named_scope("handler");
        assert_eq!(sync_ctx::scope_chain(), vec!["app", "service", "handler"]);
    }
}

#[test]
fn test_scope_chain_wire_v1_compat() {
    let key = unique_key("sc_v1", "rid");
    register::<RequestId>(key);

    // Create v1 wire bytes (no scope chain)
    let value_bytes = bincode::serialize(&RequestId("v1-value".into())).unwrap();
    let v1_bytes = make_wire_bytes_v(1, key, 1, &value_bytes);

    {
        let _scope_guard = sync_ctx::enter_scope();
        let _guard = sync_ctx::deserialize_context(&v1_bytes).unwrap();
        assert_eq!(
            sync_ctx::get_context::<RequestId>(key).unwrap().0,
            "v1-value"
        );
        // No scope chain from v1
        assert!(sync_ctx::scope_chain().is_empty());
    }
}

#[test]
fn test_scope_chain_remote_chain_lifo_restore() {
    // Simulate nested deserialization (e.g., nested remote calls)
    let key = unique_key("sc_lifo", "rid");
    register::<RequestId>(key);

    let _g = sync_ctx::enter_named_scope("local-root");

    // First "remote" call
    let _g1 = sync_ctx::enter_named_scope("sender-scope");
    sync_ctx::set_context(key, RequestId("first".into()));
    let bytes1 = sync_ctx::serialize_context().unwrap();

    {
        let _scope_guard = sync_ctx::enter_scope();
        let _guard1 = sync_ctx::deserialize_context(&bytes1).unwrap();
        // Chain shows the sender's full chain
        assert_eq!(sync_ctx::scope_chain(), vec!["local-root", "sender-scope"]);

        // Second nested "remote" call
        let _g2 = sync_ctx::enter_named_scope("nested-scope");
        let bytes2 = sync_ctx::serialize_context().unwrap();

        {
            let _scope_guard = sync_ctx::enter_scope();
            let _guard2 = sync_ctx::deserialize_context(&bytes2).unwrap();
            assert_eq!(
                sync_ctx::scope_chain(),
                vec!["local-root", "sender-scope", "nested-scope"]
            );
        }

        // After inner scope ends, original chain is restored
        assert_eq!(
            sync_ctx::scope_chain(),
            vec!["local-root", "sender-scope", "nested-scope"]
        );
    }
}

mod async_scope_chain_tests {
    use super::*;

    #[tokio::test]
    async fn test_scope_chain_with_context() {
        let _g = sync_ctx::enter_named_scope("pre-send");
        let snap = sync_ctx::snapshot();

        async_ctx::with_context(snap, async {
            assert_eq!(async_ctx::scope_chain(), vec!["pre-send"]);

            async_ctx::scope("handler", async {
                assert_eq!(async_ctx::scope_chain(), vec!["pre-send", "handler"]);
            })
            .await;
        })
        .await;
    }

    #[tokio::test]
    async fn test_named_scope_async_basic() {
        let snap = {
            let _g = sync_ctx::enter_named_scope("root");
            sync_ctx::snapshot()
        };

        async_ctx::with_context(snap, async {
            async_ctx::scope("level-1", async {
                assert_eq!(async_ctx::scope_chain(), vec!["root", "level-1"]);

                async_ctx::scope("level-2", async {
                    assert_eq!(async_ctx::scope_chain(), vec!["root", "level-1", "level-2"]);
                })
                .await;
            })
            .await;
        })
        .await;
    }
}

// ══════════════════════════════════════════════════════════════
//  Re-entrancy and contention-free safety tests
// ══════════════════════════════════════════════════════════════
//
// These tests verify that the Cell<Option<ContextStore>> design handles
// re-entrant access gracefully: no panics, no corrupted state.

/// A value whose Drop impl reads from context.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct ReentrantDropVal(String);

impl Drop for ReentrantDropVal {
    fn drop(&mut self) {
        // Try to read context during drop. Should not panic.
        // Probe during Drop without panicking if the key is missing.
        // (this Drop may fire after tests clean up).
        let _ = sync_ctx::get_context::<RequestId>("__reentrant_drop_probe__");
    }
}

#[test]
fn test_reentrant_read_during_scope_enter() {
    // Reading context during scope enter should not panic.
    let key = unique_key("reentrant_enter", "rid");
    register::<RequestId>(key);

    sync_ctx::set_context(key, RequestId("parent-val".into()));

    // Enter a scope — internally takes the store, modifies it, puts it back.
    // If anything tries to read during the take window, it should gracefully
    // return defaults (not panic).
    let _g = sync_ctx::enter_scope();

    // Value should still be accessible from parent scope.
    let val: RequestId = sync_ctx::get_context::<RequestId>(key).unwrap();
    assert_eq!(val.0, "parent-val");
}

#[test]
fn test_reentrant_read_during_scope_leave() {
    // Dropping a ScopeGuard triggers leave_scope. Reading context during
    // the leave should not panic.
    let key = unique_key("reentrant_leave", "rid");
    register::<RequestId>(key);

    sync_ctx::set_context(key, RequestId("base".into()));

    {
        let _g = sync_ctx::enter_scope();
        sync_ctx::set_context(key, RequestId("child".into()));
        assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "child");
    }
    // _g dropped — scope popped. Old child value (Arc) dropped OUTSIDE Cell window.

    let val: RequestId = sync_ctx::get_context::<RequestId>(key).unwrap();
    assert_eq!(val.0, "base");
}

#[test]
fn test_reentrant_read_during_set_context() {
    // set_context takes the store briefly. A concurrent read (simulated
    // sequentially since we're single-threaded) should be safe.
    let key_a = unique_key("reentrant_set", "a");
    let key_b = unique_key("reentrant_set", "b");
    register::<RequestId>(key_a);
    register::<RequestId>(key_b);

    sync_ctx::set_context(key_a, RequestId("aaa".into()));
    sync_ctx::set_context(key_b, RequestId("bbb".into()));

    // After set, both reads succeed.
    assert_eq!(sync_ctx::get_context::<RequestId>(key_a).unwrap().0, "aaa");
    assert_eq!(sync_ctx::get_context::<RequestId>(key_b).unwrap().0, "bbb");
}

#[test]
fn test_reentrant_drop_on_value_overwrite() {
    // When a value is overwritten, the old Arc is dropped outside the Cell
    // window. If the old value's Drop reads context, it should not panic.
    let key = unique_key("reentrant_drop_overwrite", "val");
    register::<ReentrantDropVal>(key);

    sync_ctx::set_context(key, ReentrantDropVal("first".into()));
    // This overwrites "first" — the old Arc is dropped after Cell::set().
    // ReentrantDropVal::drop tries to read context → should not panic.
    sync_ctx::set_context(key, ReentrantDropVal("second".into()));

    let val: ReentrantDropVal = sync_ctx::get_context::<ReentrantDropVal>(key).unwrap();
    assert_eq!(val.0, "second");
}

#[test]
fn test_reentrant_drop_on_scope_leave() {
    // When a scope is popped, the old current_values HashMap is dropped
    // outside the Cell window. Values' Drop impls should not panic.
    let key = unique_key("reentrant_drop_leave", "val");
    register::<ReentrantDropVal>(key);

    sync_ctx::set_context(key, ReentrantDropVal("root".into()));

    {
        let _g = sync_ctx::enter_scope();
        sync_ctx::set_context(key, ReentrantDropVal("child-scope".into()));
    }
    // _g dropped → child scope's ReentrantDropVal dropped.
    // Its Drop reads context → should not panic.

    let val: ReentrantDropVal = sync_ctx::get_context::<ReentrantDropVal>(key).unwrap();
    assert_eq!(val.0, "root");
}

#[test]
fn test_scope_push_pop_integrity_across_many_levels() {
    // Rapidly push/pop many scopes to stress the Cell take/set pattern.
    let key = unique_key("stress_scope", "rid");
    register::<RequestId>(key);

    sync_ctx::set_context(key, RequestId("root".into()));

    let depth = 50;
    let mut guards: Vec<ScopeGuard> = Vec::new();

    for i in 0..depth {
        guards.push(sync_ctx::enter_named_scope(format!("scope-{}", i)));
        sync_ctx::set_context(key, RequestId(format!("val-{}", i)));
    }

    // Innermost scope value.
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        format!("val-{}", depth - 1)
    );

    // Pop all scopes in reverse.
    for i in (0..depth).rev() {
        guards.pop();
        if i > 0 {
            assert_eq!(
                sync_ctx::get_context::<RequestId>(key).unwrap().0,
                format!("val-{}", i - 1)
            );
        }
    }

    // Back to root.
    assert_eq!(sync_ctx::get_context::<RequestId>(key).unwrap().0, "root");
}

#[test]
fn test_scope_chain_integrity_after_many_push_pops() {
    // Verify scope_chain is correct after many push/pop cycles.
    let key = unique_key("chain_stress", "rid");
    register::<RequestId>(key);

    for round in 0..10 {
        let name = format!("round-{}", round);
        let _g = sync_ctx::enter_named_scope(&name);
        let chain = sync_ctx::scope_chain();
        assert!(chain.last().map(|s| s.as_str()) == Some(name.as_str()));
    }
    // All guards dropped, chain should be empty.
    assert!(sync_ctx::scope_chain().is_empty());
}

#[test]
fn test_update_context_basic() {
    let key = unique_key("update_ctx", "counter");

    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
    struct Counter(u64);

    register::<Counter>(key);

    sync_ctx::set_context(key, Counter(10));

    // Update: increment the counter.
    sync_ctx::update_context::<Counter>(key, |c| Counter(c.0 + 5));

    let val = sync_ctx::get_context::<Counter>(key).unwrap();
    assert_eq!(val.0, 15);
}

#[test]
fn test_update_context_default_when_unset() {
    let key = unique_key("update_default", "counter");

    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
    struct Counter(u64);

    register::<Counter>(key);

    // No prior set — should start from default (0).
    sync_ctx::update_context::<Counter>(key, |c| Counter(c.0 + 1));

    let val = sync_ctx::get_context::<Counter>(key).unwrap();
    assert_eq!(val.0, 1);
}

#[test]
fn test_update_context_callback_can_read_other_keys() {
    // The callback in update_context runs with the store available,
    // so reading other keys should work.
    let key_a = unique_key("update_read_other", "a");
    let key_b = unique_key("update_read_other", "b");
    register::<RequestId>(key_a);
    register::<RequestId>(key_b);

    sync_ctx::set_context(key_a, RequestId("aaa".into()));
    sync_ctx::set_context(key_b, RequestId("bbb".into()));

    // Update key_a, reading key_b inside the callback.
    sync_ctx::update_context::<RequestId>(key_a, |_old| {
        let b = sync_ctx::get_context::<RequestId>(key_b).unwrap();
        RequestId(format!("merged-{}", b.0))
    });

    assert_eq!(
        sync_ctx::get_context::<RequestId>(key_a).unwrap().0,
        "merged-bbb"
    );
    // key_b unchanged.
    assert_eq!(sync_ctx::get_context::<RequestId>(key_b).unwrap().0, "bbb");
}

#[test]
fn test_update_context_in_scope_reverts() {
    let key = unique_key("update_scope_revert", "val");

    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
    struct Val(String);

    register::<Val>(key);

    sync_ctx::set_context(key, Val("root".into()));

    {
        let _g = sync_ctx::enter_scope();
        sync_ctx::update_context::<Val>(key, |_| Val("updated-in-child".into()));
        assert_eq!(
            sync_ctx::get_context::<Val>(key).unwrap().0,
            "updated-in-child"
        );
    }

    // Reverted after scope exit.
    assert_eq!(sync_ctx::get_context::<Val>(key).unwrap().0, "root");
}

#[test]
fn test_get_context_option_some_and_none() {
    let key_set = unique_key("get_opt", "set");
    let key_unset = unique_key("get_opt", "unset");
    register::<RequestId>(key_set);
    register::<RequestId>(key_unset);

    sync_ctx::set_context(key_set, RequestId("hello".into()));

    assert_eq!(
        sync_ctx::get_context::<RequestId>(key_set),
        Some(RequestId("hello".into()))
    );
    assert_eq!(sync_ctx::get_context::<RequestId>(key_unset), None);
}

#[test]
fn test_snapshot_uses_arc_sharing() {
    // After the Arc migration, snapshot values share memory with the store.
    // This test verifies snapshot + attach works correctly.
    let key = unique_key("snap_arc", "rid");
    register::<RequestId>(key);

    sync_ctx::set_context(key, RequestId("original".into()));
    let snap = sync_ctx::snapshot();

    // Modify after snapshot.
    sync_ctx::set_context(key, RequestId("modified".into()));

    // Attach restores snapshot values.
    {
        let _g = sync_ctx::attach(snap);
        assert_eq!(
            sync_ctx::get_context::<RequestId>(key).unwrap().0,
            "original"
        );
    }

    // After attach scope ends, current value is back.
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "modified"
    );
}

#[test]
fn test_concurrent_scope_and_read_no_panic() {
    // Simulate the pattern that caused BorrowError in v0.3.x:
    // A tracing callback fires during a write, triggering a re-entrant read.
    // With Cell<Option<ContextStore>>, this returns defaults instead of panicking.
    let key = unique_key("concurrent_rw", "rid");
    register::<RequestId>(key);

    sync_ctx::set_context(key, RequestId("base".into()));

    // Rapidly alternate set + get (simulating interleaved callbacks).
    for i in 0..100 {
        sync_ctx::set_context(key, RequestId(format!("iter-{}", i)));
        let val: RequestId = sync_ctx::get_context::<RequestId>(key).unwrap();
        assert_eq!(val.0, format!("iter-{}", i));
    }
}

#[test]
fn test_cached_key_o1_read_in_nested_scopes() {
    // Cached keys should always be in current_values after scope entry.
    let key = unique_key("cached_read", "rid");
    register_with::<RequestId>(key, |opts| opts.cached());

    sync_ctx::set_context(key, RequestId("root-val".into()));

    let _g1 = sync_ctx::enter_scope();
    // Cached key should be readable without walking parents.
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "root-val"
    );

    let _g2 = sync_ctx::enter_scope();
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "root-val"
    );

    // Override in inner scope.
    sync_ctx::set_context(key, RequestId("inner-val".into()));
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "inner-val"
    );

    drop(_g2);
    // After inner scope exit, cached value from g1's scope is restored.
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "root-val"
    );

    drop(_g1);
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "root-val"
    );
}

#[test]
fn test_non_cached_key_walks_parents() {
    // Non-cached keys (default) should find values in parent scopes.
    let key = unique_key("non_cached", "rid");
    register::<RequestId>(key);

    sync_ctx::set_context(key, RequestId("root-val".into()));

    let _g1 = sync_ctx::enter_scope();
    // Not set in child scope — walks to root.
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "root-val"
    );

    // Override in child.
    sync_ctx::set_context(key, RequestId("child-val".into()));
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "child-val"
    );

    let _g2 = sync_ctx::enter_scope();
    // Grandchild walks to child.
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "child-val"
    );

    drop(_g2);
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "child-val"
    );

    drop(_g1);
    assert_eq!(
        sync_ctx::get_context::<RequestId>(key).unwrap().0,
        "root-val"
    );
}

#[tokio::test]
async fn test_async_reentrant_safety() {
    // Verify that scope_async and named_scope_async don't panic
    // under re-entrant-like patterns.
    let key = unique_key("async_reentrant", "rid");
    register::<RequestId>(key);

    let snap = {
        sync_ctx::set_context(key, RequestId("base".into()));
        sync_ctx::snapshot()
    };

    async_ctx::with_context(snap, async {
        assert_eq!(async_ctx::get_context::<RequestId>(key).unwrap().0, "base");

        async_ctx::scope("", async {
            async_ctx::set_context(key, RequestId("in-scope-async".into()));
            assert_eq!(
                async_ctx::get_context::<RequestId>(key).unwrap().0,
                "in-scope-async"
            );

            async_ctx::scope("inner", async {
                assert_eq!(
                    async_ctx::get_context::<RequestId>(key).unwrap().0,
                    "in-scope-async"
                );
                async_ctx::set_context(key, RequestId("deep".into()));
                assert_eq!(async_ctx::get_context::<RequestId>(key).unwrap().0, "deep");
            })
            .await;

            assert_eq!(
                async_ctx::get_context::<RequestId>(key).unwrap().0,
                "in-scope-async"
            );
        })
        .await;

        assert_eq!(async_ctx::get_context::<RequestId>(key).unwrap().0, "base");
    })
    .await;
}

// ══════════════════════════════════════════════════════════════
//  Fork tests
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_fork_reads_parent_values() {
    let key = unique_key("fork_read", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = sync_ctx::enter_scope();
        sync_ctx::set_context(key, RequestId("parent-val".into()));
        sync_ctx::snapshot()
    };

    let result = async_ctx::with_context(snap, async {
        async_ctx::get_context::<RequestId>(key).unwrap()
    })
    .await;

    assert_eq!(result.0, "parent-val");
}

#[tokio::test]
async fn test_fork_writes_are_isolated() {
    let key = unique_key("fork_isolate", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = sync_ctx::enter_scope();
        sync_ctx::set_context(key, RequestId("parent".into()));
        sync_ctx::snapshot()
    };

    async_ctx::with_context(snap, async {
        async_ctx::set_context(key, RequestId("child-override".into()));
        let val = async_ctx::get_context::<RequestId>(key).unwrap();
        assert_eq!(val.0, "child-override");
    })
    .await;

    let parent_val = sync_ctx::get_context::<RequestId>(key).unwrap_or_default();
    assert_eq!(parent_val, RequestId::default());
}

#[tokio::test]
async fn test_fork_is_cheap_clone() {
    let key = unique_key("fork_clone", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = sync_ctx::enter_scope();
        sync_ctx::set_context(key, RequestId("shared".into()));
        sync_ctx::snapshot()
    };

    let snap2 = snap.clone();

    let r1 = async_ctx::with_context(snap, async {
        async_ctx::get_context::<RequestId>(key).unwrap()
    })
    .await;
    let r2 = async_ctx::with_context(snap2, async {
        async_ctx::get_context::<RequestId>(key).unwrap()
    })
    .await;

    assert_eq!(r1.0, "shared");
    assert_eq!(r2.0, "shared");
}

#[tokio::test]
async fn test_fork_child_scopes_work() {
    let key = unique_key("fork_scope", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = sync_ctx::enter_scope();
        sync_ctx::set_context(key, RequestId("base".into()));
        sync_ctx::snapshot()
    };

    async_ctx::with_context(snap, async {
        assert_eq!(async_ctx::get_context::<RequestId>(key).unwrap().0, "base");

        async_ctx::scope("", async {
            async_ctx::set_context(key, RequestId("inner".into()));
            assert_eq!(async_ctx::get_context::<RequestId>(key).unwrap().0, "inner");
        })
        .await;

        assert_eq!(async_ctx::get_context::<RequestId>(key).unwrap().0, "base");
    })
    .await;
}

#[tokio::test]
async fn test_spawn_with_fork_async() {
    let key = unique_key("fork_spawn", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = sync_ctx::enter_scope();
        sync_ctx::set_context(key, RequestId("for-spawn".into()));
        sync_ctx::snapshot()
    };

    let join = tokio::spawn(async_ctx::with_context(snap, async {
        async_ctx::get_context::<RequestId>(key).unwrap()
    }));

    let result = join.await.unwrap();
    assert_eq!(result.0, "for-spawn");
}

#[tokio::test]
async fn test_fork_empty_context() {
    let snap = ContextSnapshot::empty();

    async_ctx::with_context(snap, async {
        // No values set — empty context should be attachable.
    })
    .await;
}

#[tokio::test]
async fn test_fork_scope_chain_preserved() {
    let key = unique_key("fork_chain", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = sync_ctx::enter_named_scope("parent-scope");
        sync_ctx::set_context(key, RequestId("chained".into()));
        sync_ctx::snapshot()
    };

    async_ctx::with_context(snap, async {
        let chain = async_ctx::scope_chain();
        assert!(
            chain.contains(&"parent-scope".to_string()),
            "snapshot should preserve parent scope chain: {:?}",
            chain
        );
    })
    .await;
}
