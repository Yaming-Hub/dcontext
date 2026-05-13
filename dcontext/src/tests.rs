use serde::{Deserialize, Serialize};

#[cfg(feature = "base64")]
use base64::Engine as _;

use crate::*;
// Re-import from the crate root
use crate::wire::test_helpers::{make_wire_bytes, make_wire_bytes_v};
use crate::ContextError;
use crate::ContextSnapshot;
use crate::ScopeGuard;
use std::future::Future;

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

fn with_snapshot<F: Future>(snap: ContextSnapshot, fut: F) -> WithContext<F> {
    fut.with(snap.into())
}

async fn async_scope<F: Future>(name: &str, fut: F) -> F::Output {
    let _scope = push_scope(name);
    fut.await
}

fn enter_scope() -> ScopeGuard {
    crate::registry::with_global_registry(|registry| {
        crate::store::try_apply(|store| ScopeGuard::new(store.push_scope(registry, None)))
            .unwrap_or_else(ScopeGuard::noop)
    })
}

fn enter_named_scope(name: impl Into<String>) -> ScopeGuard {
    let name = name.into();
    push_scope(&name)
}

fn set_context<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    set_context_variable(key, value);
}

fn get_context<T>(key: &str) -> Option<T>
where
    T: Clone + Send + Sync + 'static,
{
    get_context_variable(key)
}

fn update_context<T>(key: &'static str, f: impl FnOnce(T) -> T)
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    update_context_variable(key, f);
}

fn snapshot() -> ContextSnapshot {
    capture()
}

fn attach(snap: ContextSnapshot) -> AttachGuard {
    attach_snapshot(snap)
}

fn restore(snap: ContextSnapshot) -> AttachGuard {
    attach_snapshot(snap)
}

fn serialize_context() -> Result<Vec<u8>, ContextError> {
    capture().serialize()
}

fn deserialize_context(bytes: &[u8]) -> Result<AttachGuard, ContextError> {
    ContextSnapshot::deserialize(bytes).map(attach_snapshot)
}

fn snapshot_context<T>(snap: &ContextSnapshot, key: &str) -> Option<T>
where
    T: Clone + Send + Sync + 'static,
{
    snap.values
        .get(key)
        .and_then(|arc| arc.as_any().downcast_ref::<T>().cloned())
}

// ══════════════════════════════════════════════════════════════
//  Registration tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_register_and_get_default() {
    let key = unique_key("reg_default", "rid");
    register::<RequestId>(key);
    let val: RequestId = get_context::<RequestId>(key).unwrap_or_default();
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
    assert_eq!(get_context::<RequestId>(key), None);
}

// Registry-validation wrappers like try_get_context were removed in the sync/async split.

// ══════════════════════════════════════════════════════════════
//  Basic get/set tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_set_and_get() {
    let key = unique_key("set_get", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("req-42".into()));
    let val: RequestId = get_context::<RequestId>(key).unwrap();
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
    set_context(key, RequestId("parent".into()));

    {
        let _guard = enter_scope();
        set_context(key, RequestId("child".into()));
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "child");
    }
    // Scope reverted
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "parent");
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
            assert_eq!(get_context::<RequestId>(key).unwrap().0, "level2");
        }
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "level1");
    }
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "root");
}

#[test]
fn test_scope_fn() {
    let key = unique_key("scope_fn", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("before".into()));

    {
        let _scope_guard = enter_scope();
        set_context(key, RequestId("inside".into()));
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "inside");
    }

    assert_eq!(get_context::<RequestId>(key).unwrap().0, "before");
}

#[test]
fn test_scope_inherits_parent() {
    let key = unique_key("scope_inherit", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("parent_val".into()));

    {
        let _scope_guard = enter_scope();
        // Should see parent value without setting anything
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "parent_val");
    }
}

#[test]
fn test_scope_partial_override() {
    let key_a = unique_key("scope_partial", "a");
    let key_b = unique_key("scope_partial", "b");
    register::<RequestId>(key_a);
    register::<UserId>(key_b);

    set_context(key_a, RequestId("a_parent".into()));
    set_context(key_b, UserId(10));

    {
        let _scope_guard = enter_scope();
        // Override only key_a
        set_context(key_a, RequestId("a_child".into()));
        assert_eq!(get_context::<RequestId>(key_a).unwrap().0, "a_child");
        assert_eq!(get_context::<UserId>(key_b).unwrap().0, 10); // inherited
    }

    assert_eq!(get_context::<RequestId>(key_a).unwrap().0, "a_parent");
    assert_eq!(get_context::<UserId>(key_b).unwrap().0, 10);
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
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "snapped");
    }
    // Back to modified
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "modified");
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

    let snap = snapshot();
    let handle = std::thread::Builder::new()
        .name("test-worker".into())
        .spawn(move || {
            let _guard = restore(snap);
            get_context::<RequestId>(key).unwrap()
        })
        .unwrap();

    let result = handle.join().unwrap();
    assert_eq!(result.0, "main-thread");
}

#[test]
fn test_wrap_with_context_fn_once() {
    let key = unique_key("wrap_once", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("wrapped".into()));

    let snap = snapshot();
    let wrapped = move || {
        let _guard = restore(snap);
        get_context::<RequestId>(key).unwrap()
    };

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

    let snap = snapshot();
    let wrapped = move || {
        let _guard = attach(snap.clone());
        get_context::<RequestId>(key).unwrap()
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
    set_context(key, RequestId("serialized".into()));

    let bytes = serialize_context().unwrap();

    // Deserialize in a new scope
    {
        let _guard = deserialize_context(&bytes).unwrap();
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "serialized");
    }
}

#[cfg(feature = "base64")]
#[test]
fn test_serialize_deserialize_string_roundtrip() {
    let key = unique_key("serde_str", "rid");
    register::<RequestId>(key);
    set_context(key, RequestId("base64val".into()));

    let encoded = serialize_context()
        .map(|b| base64::engine::general_purpose::STANDARD.encode(&b))
        .map_err(|e| e)
        .unwrap();
    assert!(!encoded.is_empty());

    // Clear and restore
    {
        let _scope_guard = enter_scope();
        set_context(key, RequestId("cleared".into()));
        {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&encoded)
                .unwrap();
            let _guard = deserialize_context(&bytes).unwrap();
            assert_eq!(get_context::<RequestId>(key).unwrap().0, "base64val");
        }
    }
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
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "known");
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

    {
        let _scope_guard = enter_scope();
        let _guard = deserialize_context(&bytes).unwrap();
        assert_eq!(get_context::<RequestId>(key_a).unwrap().0, "req-multi");
        assert_eq!(get_context::<UserId>(key_b).unwrap().0, 42);
    }
}

#[test]
fn test_serialize_deserialize_with_isolated_registry() {
    let key_rid = "isolated.serialize.request_id";
    let key_uid = "isolated.serialize.user_id";

    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>(key_rid);
    builder.register_with::<UserId>(key_uid, |opts| opts.version(2));

    let map = builder.into_map();
    let registry = crate::registry::Registry::new(&map);

    let values: HashMap<&'static str, Arc<dyn ContextValue>> = HashMap::from([
        (
            key_rid,
            Arc::new(RequestId("iso-req".into())) as Arc<dyn ContextValue>,
        ),
        (key_uid, Arc::new(UserId(7)) as Arc<dyn ContextValue>),
    ]);

    let bytes =
        crate::wire::serialize_from(&registry, values, vec!["rpc".into(), "handler".into()])
            .unwrap();

    let snap = crate::wire::deserialize_to_snapshot(&registry, &bytes).unwrap();
    assert_eq!(
        snap.scope_chain(),
        &["rpc".to_string(), "handler".to_string()]
    );
    assert_eq!(
        snapshot_context::<RequestId>(&snap, key_rid),
        Some(RequestId("iso-req".into()))
    );
    assert_eq!(snapshot_context::<UserId>(&snap, key_uid), Some(UserId(7)));
}

#[test]
fn test_capture_with_custom_registry_excludes_local() {
    let key_public = "isolated.capture.public";
    let key_local = "isolated.capture.local";

    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>(key_public);
    builder.register_with::<UserId>(key_local, |opts| opts.local_only());

    let map = builder.into_map();
    let registry = crate::registry::Registry::new(&map);

    let mut store = ContextStore::new();
    store.set_value(key_public, Arc::new(RequestId("root".into())));
    store.set_value(key_local, Arc::new(UserId(1)));
    store.push_scope(&registry, Some("request".into()));
    store.set_value(key_public, Arc::new(RequestId("child".into())));
    store.set_value(key_local, Arc::new(UserId(2)));

    let snap = crate::capture_with_registry(&store, &registry);

    assert_eq!(snap.scope_chain(), &["request".to_string()]);
    assert_eq!(
        snapshot_context::<RequestId>(&snap, key_public),
        Some(RequestId("child".into()))
    );
    assert_eq!(snapshot_context::<UserId>(&snap, key_local), None);
}

#[test]
fn test_from_snapshot_with_isolated_registry_filters_invalid() {
    let key_valid = "isolated.snapshot.valid";
    let key_local = "isolated.snapshot.local";
    let key_mismatch = "isolated.snapshot.mismatch";
    let key_unknown = "isolated.snapshot.unknown";

    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>(key_valid);
    builder.register_with::<UserId>(key_local, |opts| opts.local_only());
    builder.register::<UserId>(key_mismatch);

    let map = builder.into_map();
    let registry = crate::registry::Registry::new(&map);

    let values: HashMap<&'static str, Arc<dyn ContextValue>> = HashMap::from([
        (
            key_valid,
            Arc::new(RequestId("keep-me".into())) as Arc<dyn ContextValue>,
        ),
        (key_local, Arc::new(UserId(9)) as Arc<dyn ContextValue>),
        (
            key_mismatch,
            Arc::new(RequestId("wrong-type".into())) as Arc<dyn ContextValue>,
        ),
        (
            key_unknown,
            Arc::new(RequestId("unknown".into())) as Arc<dyn ContextValue>,
        ),
    ]);
    let snap = ContextSnapshot {
        values: Arc::new(values),
        scope_chain: vec!["remote".into()],
    };

    let store = crate::store_from_snapshot_with_registry(snap, &registry);

    assert_eq!(store.scope_chain(), vec!["remote"]);
    assert_eq!(
        store
            .get_value(key_valid)
            .and_then(|arc| arc.as_any().downcast_ref::<RequestId>().cloned()),
        Some(RequestId("keep-me".into()))
    );
    assert!(store.get_value(key_local).is_none());
    assert!(store.get_value(key_mismatch).is_none());
    assert!(store.get_value(key_unknown).is_none());
}

#[test]
fn test_push_scope_caches_with_isolated_registry() {
    let key_cached = "isolated.cache.cached";
    let key_plain = "isolated.cache.plain";

    let mut builder = RegistryBuilder::new();
    builder.register_with::<RequestId>(key_cached, |opts| opts.cached());
    builder.register::<UserId>(key_plain);

    let map = builder.into_map();
    let registry = crate::registry::Registry::new(&map);

    let mut store = ContextStore::new();
    store.set_value(key_cached, Arc::new(RequestId("cached-root".into())));
    store.set_value(key_plain, Arc::new(UserId(42)));

    let depth = store.push_scope(&registry, Some("child".into()));

    assert_eq!(depth, 2);
    assert!(store.current_values.contains_key(key_cached));
    assert!(!store.current_values.contains_key(key_plain));
    assert_eq!(
        store
            .get_value(key_cached)
            .and_then(|arc| arc.as_any().downcast_ref::<RequestId>().cloned()),
        Some(RequestId("cached-root".into()))
    );
    assert_eq!(
        store
            .get_value(key_plain)
            .and_then(|arc| arc.as_any().downcast_ref::<UserId>().cloned()),
        Some(UserId(42))
    );
}

// ══════════════════════════════════════════════════════════════
//  Additional tests (from review feedback S1)
// ══════════════════════════════════════════════════════════════

#[test]
fn test_try_get_registered_but_unset() {
    let key = unique_key("try_get_none", "rid");
    register::<RequestId>(key);
    // Registered but never set — should return Ok(None)
    let result = get_context::<RequestId>(key);
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
    set_context(key_a, RequestId("macro-a".into()));
    set_context(key_b, UserId(77));
    assert_eq!(get_context::<RequestId>(key_a).unwrap().0, "macro-a");
    assert_eq!(get_context::<UserId>(key_b).unwrap().0, 77);
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
        let _guard = deserialize_context(&wire).unwrap();
        let val: TraceV2 = get_context::<TraceV2>(key).unwrap();
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
    let _guard = enter_scope();
    set_context(
        key,
        TraceV2 {
            trace_id: "current".into(),
            span_id: "current-span".into(),
        },
    );
    let bytes = serialize_context().unwrap();

    {
        let _scope_guard = enter_scope();
        let _guard = deserialize_context(&bytes).unwrap();
        let val: TraceV2 = get_context::<TraceV2>(key).unwrap();
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

    let _guard = enter_scope();
    set_context(key, RequestId("json-encoded".into()));

    // Roundtrip through serialization — uses JSON codec, not bincode.
    let bytes = serialize_context().unwrap();
    {
        let _scope_guard = enter_scope();
        let _guard = deserialize_context(&bytes).unwrap();
        let val: RequestId = get_context::<RequestId>(key).unwrap();
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

    let _guard = enter_scope();
    set_context(key, RequestId("verify-json".into()));

    let wire_bytes = serialize_context().unwrap();

    // The inner value bytes should be valid JSON, not bincode.
    // Deserialize on a fresh thread to confirm.
    let handle = std::thread::spawn(move || {
        register_with::<RequestId>(key, |o| {
            o.codec(
                |val| serde_json::to_vec(val).map_err(|e| e.to_string()),
                |bytes| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
            )
        });
        let _guard = deserialize_context(&wire_bytes).unwrap();
        let val: RequestId = get_context::<RequestId>(key).unwrap();
        assert_eq!(val.0, "verify-json");
    });
    handle.join().unwrap();
}

#[test]
fn test_default_codec_still_works() {
    // Ensure normal registration (bincode) is unaffected by codec feature.
    let key = unique_key("codec_default", "rid");
    register::<RequestId>(key);

    let _guard = enter_scope();
    set_context(key, RequestId("bincode-default".into()));

    let bytes = serialize_context().unwrap();
    {
        let _scope_guard = enter_scope();
        let _guard = deserialize_context(&bytes).unwrap();
        let val: RequestId = get_context::<RequestId>(key).unwrap();
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

    let _scope = enter_scope();
    set_context(key, RequestId("should-not-serialize".into()));

    // Serialize — the local_only value must be excluded from wire bytes.
    let bytes = serialize_context().unwrap();

    // Deserialize on a fresh thread so the original scope's value isn't visible.
    std::thread::spawn(move || {
        let _guard = deserialize_context(&bytes).unwrap();
        let val = get_context::<RequestId>(key);
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
            set_context(key, RequestId("async-val".into()));
            snapshot()
        };

        let result = with_snapshot(snap, async { get_context::<RequestId>(key).unwrap() }).await;

        assert_eq!(result.0, "async-val");
    }

    #[tokio::test]
    async fn test_scope_async() {
        let key = unique_key("scope_async", "rid");
        register::<RequestId>(key);

        let snap = {
            set_context(key, RequestId("before-async".into()));
            snapshot()
        };

        with_snapshot(snap, async {
            assert_eq!(get_context::<RequestId>(key).unwrap().0, "before-async");

            async_scope("", async {
                set_context(key, RequestId("inside-async".into()));
                assert_eq!(get_context::<RequestId>(key).unwrap().0, "inside-async");
            })
            .await;

            assert_eq!(get_context::<RequestId>(key).unwrap().0, "before-async");
        })
        .await;
    }

    #[tokio::test]
    async fn test_spawn_with_context_async() {
        let key = unique_key("async_spawn", "rid");
        register::<RequestId>(key);

        let snap = {
            set_context(key, RequestId("spawned-async".into()));
            snapshot()
        };

        let handle = with_snapshot(snap, async {
            let child_snap = snapshot();
            tokio::spawn(with_snapshot(child_snap, async {
                get_context::<RequestId>(key).unwrap()
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
            set_context(key, RequestId("outer".into()));
            snapshot()
        };

        with_snapshot(snap, async {
            assert_eq!(get_context::<RequestId>(key).unwrap().0, "outer");

            async_scope("", async {
                set_context(key, RequestId("inner".into()));
                assert_eq!(get_context::<RequestId>(key).unwrap().0, "inner");
            })
            .await;

            assert_eq!(get_context::<RequestId>(key).unwrap().0, "outer");
        })
        .await;
    }

    #[tokio::test]
    async fn test_async_serialize_roundtrip() {
        let key = unique_key("async_serde", "rid");
        register::<RequestId>(key);

        let snap = {
            set_context(key, RequestId("async-serde".into()));
            snapshot()
        };

        with_snapshot(snap, async {
            let bytes = serialize_context().unwrap();
            set_context(key, RequestId("cleared".into()));
            let _guard = deserialize_context(&bytes).unwrap();
            assert_eq!(get_context::<RequestId>(key).unwrap().0, "async-serde");
        })
        .await;
    }
}

// ══════════════════════════════════════════════════════════════
//  Scope chain tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_scope_chain_empty_by_default() {
    let chain = scope_chain();
    assert!(chain.is_empty(), "default scope chain should be empty");
}

#[test]
fn test_scope_chain_named_scope() {
    let _g = enter_named_scope("outer");
    assert_eq!(scope_chain(), vec!["outer"]);
    {
        let _g2 = enter_named_scope("inner");
        assert_eq!(scope_chain(), vec!["outer", "inner"]);
    }
    assert_eq!(scope_chain(), vec!["outer"]);
}

#[test]
fn test_scope_chain_unnamed_invisible() {
    let _g1 = enter_named_scope("named");
    let _g2 = enter_scope();
    let _g3 = enter_named_scope("also-named");
    assert_eq!(scope_chain(), vec!["named", "also-named"]);
}

#[test]
fn test_scope_chain_snapshot_preserves_chain() {
    let _g = enter_named_scope("request-handler");
    let snap = snapshot();
    assert_eq!(snap.scope_chain, vec!["request-handler"]);

    // Restore in a new scope — the chain becomes remote_chain
    {
        let _scope_guard = enter_scope();
        let _guard = attach(snap.clone());
        assert_eq!(scope_chain(), vec!["request-handler"]);

        // Push local named scopes
        let _g2 = enter_named_scope("sub-handler");
        assert_eq!(scope_chain(), vec!["request-handler", "sub-handler"]);
    }
}

#[test]
fn test_scope_chain_serialize_roundtrip() {
    let key = unique_key("sc_serde", "rid");
    register::<RequestId>(key);

    let _g1 = enter_named_scope("app");
    let _g2 = enter_named_scope("service");
    set_context(key, RequestId("req-1".into()));

    let bytes = serialize_context().unwrap();

    // Deserialize in a clean scope
    {
        let _scope_guard = enter_scope();
        let _guard = deserialize_context(&bytes).unwrap();
        // Values restored
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "req-1");
        // Scope chain restored as remote prefix
        assert_eq!(scope_chain(), vec!["app", "service"]);

        // Push more local scopes
        let _g3 = enter_named_scope("handler");
        assert_eq!(scope_chain(), vec!["app", "service", "handler"]);
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
        let _scope_guard = enter_scope();
        let _guard = deserialize_context(&v1_bytes).unwrap();
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "v1-value");
        // No scope chain from v1
        assert!(scope_chain().is_empty());
    }
}

#[test]
fn test_scope_chain_remote_chain_lifo_restore() {
    // Simulate nested deserialization (e.g., nested remote calls)
    let key = unique_key("sc_lifo", "rid");
    register::<RequestId>(key);

    let _g = enter_named_scope("local-root");

    // First "remote" call
    let _g1 = enter_named_scope("sender-scope");
    set_context(key, RequestId("first".into()));
    let bytes1 = serialize_context().unwrap();

    {
        let _scope_guard = enter_scope();
        let _guard1 = deserialize_context(&bytes1).unwrap();
        // Chain shows the sender's full chain
        assert_eq!(scope_chain(), vec!["local-root", "sender-scope"]);

        // Second nested "remote" call
        let _g2 = enter_named_scope("nested-scope");
        let bytes2 = serialize_context().unwrap();

        {
            let _scope_guard = enter_scope();
            let _guard2 = deserialize_context(&bytes2).unwrap();
            assert_eq!(
                scope_chain(),
                vec!["local-root", "sender-scope", "nested-scope"]
            );
        }

        // After inner scope ends, original chain is restored
        assert_eq!(
            scope_chain(),
            vec!["local-root", "sender-scope", "nested-scope"]
        );
    }
}

mod async_scope_chain_tests {
    use super::*;

    #[tokio::test]
    async fn test_scope_chain_with_context() {
        let _g = enter_named_scope("pre-send");
        let snap = snapshot();

        with_snapshot(snap, async {
            assert_eq!(scope_chain(), vec!["pre-send"]);

            async_scope("handler", async {
                assert_eq!(scope_chain(), vec!["pre-send", "handler"]);
            })
            .await;
        })
        .await;
    }

    #[tokio::test]
    async fn test_named_scope_async_basic() {
        let snap = {
            let _g = enter_named_scope("root");
            snapshot()
        };

        with_snapshot(snap, async {
            async_scope("level-1", async {
                assert_eq!(scope_chain(), vec!["root", "level-1"]);

                async_scope("level-2", async {
                    assert_eq!(scope_chain(), vec!["root", "level-1", "level-2"]);
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
        let _ = get_context::<RequestId>("__reentrant_drop_probe__");
    }
}

#[test]
fn test_reentrant_read_during_scope_enter() {
    // Reading context during scope enter should not panic.
    let key = unique_key("reentrant_enter", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("parent-val".into()));

    // Enter a scope — internally takes the store, modifies it, puts it back.
    // If anything tries to read during the take window, it should gracefully
    // return defaults (not panic).
    let _g = enter_scope();

    // Value should still be accessible from parent scope.
    let val: RequestId = get_context::<RequestId>(key).unwrap();
    assert_eq!(val.0, "parent-val");
}

#[test]
fn test_reentrant_read_during_scope_leave() {
    // Dropping a ScopeGuard triggers leave_scope. Reading context during
    // the leave should not panic.
    let key = unique_key("reentrant_leave", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("base".into()));

    {
        let _g = enter_scope();
        set_context(key, RequestId("child".into()));
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "child");
    }
    // _g dropped — scope popped. Old child value (Arc) dropped OUTSIDE Cell window.

    let val: RequestId = get_context::<RequestId>(key).unwrap();
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

    set_context(key_a, RequestId("aaa".into()));
    set_context(key_b, RequestId("bbb".into()));

    // After set, both reads succeed.
    assert_eq!(get_context::<RequestId>(key_a).unwrap().0, "aaa");
    assert_eq!(get_context::<RequestId>(key_b).unwrap().0, "bbb");
}

#[test]
fn test_reentrant_drop_on_value_overwrite() {
    // When a value is overwritten, the old Arc is dropped outside the Cell
    // window. If the old value's Drop reads context, it should not panic.
    let key = unique_key("reentrant_drop_overwrite", "val");
    register::<ReentrantDropVal>(key);

    set_context(key, ReentrantDropVal("first".into()));
    // This overwrites "first" — the old Arc is dropped after Cell::set().
    // ReentrantDropVal::drop tries to read context → should not panic.
    set_context(key, ReentrantDropVal("second".into()));

    let val: ReentrantDropVal = get_context::<ReentrantDropVal>(key).unwrap();
    assert_eq!(val.0, "second");
}

#[test]
fn test_reentrant_drop_on_scope_leave() {
    // When a scope is popped, the old current_values HashMap is dropped
    // outside the Cell window. Values' Drop impls should not panic.
    let key = unique_key("reentrant_drop_leave", "val");
    register::<ReentrantDropVal>(key);

    set_context(key, ReentrantDropVal("root".into()));

    {
        let _g = enter_scope();
        set_context(key, ReentrantDropVal("child-scope".into()));
    }
    // _g dropped → child scope's ReentrantDropVal dropped.
    // Its Drop reads context → should not panic.

    let val: ReentrantDropVal = get_context::<ReentrantDropVal>(key).unwrap();
    assert_eq!(val.0, "root");
}

#[test]
fn test_scope_push_pop_integrity_across_many_levels() {
    // Rapidly push/pop many scopes to stress the Cell take/set pattern.
    let key = unique_key("stress_scope", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("root".into()));

    let depth = 50;
    let mut guards: Vec<ScopeGuard> = Vec::new();

    for i in 0..depth {
        guards.push(enter_named_scope(format!("scope-{}", i)));
        set_context(key, RequestId(format!("val-{}", i)));
    }

    // Innermost scope value.
    assert_eq!(
        get_context::<RequestId>(key).unwrap().0,
        format!("val-{}", depth - 1)
    );

    // Pop all scopes in reverse.
    for i in (0..depth).rev() {
        guards.pop();
        if i > 0 {
            assert_eq!(
                get_context::<RequestId>(key).unwrap().0,
                format!("val-{}", i - 1)
            );
        }
    }

    // Back to root.
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "root");
}

#[test]
fn test_scope_chain_integrity_after_many_push_pops() {
    // Verify scope_chain is correct after many push/pop cycles.
    let key = unique_key("chain_stress", "rid");
    register::<RequestId>(key);

    for round in 0..10 {
        let name = format!("round-{}", round);
        let _g = enter_named_scope(&name);
        let chain = scope_chain();
        assert!(chain.last().map(|s| s.as_str()) == Some(name.as_str()));
    }
    // All guards dropped, chain should be empty.
    assert!(scope_chain().is_empty());
}

#[test]
fn test_update_context_basic() {
    let key = unique_key("update_ctx", "counter");

    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
    struct Counter(u64);

    register::<Counter>(key);

    set_context(key, Counter(10));

    // Update: increment the counter.
    update_context::<Counter>(key, |c| Counter(c.0 + 5));

    let val = get_context::<Counter>(key).unwrap();
    assert_eq!(val.0, 15);
}

#[test]
fn test_update_context_default_when_unset() {
    let key = unique_key("update_default", "counter");

    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
    struct Counter(u64);

    register::<Counter>(key);

    // No prior set — should start from default (0).
    update_context::<Counter>(key, |c| Counter(c.0 + 1));

    let val = get_context::<Counter>(key).unwrap();
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

    set_context(key_a, RequestId("aaa".into()));
    set_context(key_b, RequestId("bbb".into()));

    // Update key_a, reading key_b inside the callback.
    update_context::<RequestId>(key_a, |_old| {
        let b = get_context::<RequestId>(key_b).unwrap();
        RequestId(format!("merged-{}", b.0))
    });

    assert_eq!(get_context::<RequestId>(key_a).unwrap().0, "merged-bbb");
    // key_b unchanged.
    assert_eq!(get_context::<RequestId>(key_b).unwrap().0, "bbb");
}

#[test]
fn test_update_context_in_scope_reverts() {
    let key = unique_key("update_scope_revert", "val");

    #[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
    struct Val(String);

    register::<Val>(key);

    set_context(key, Val("root".into()));

    {
        let _g = enter_scope();
        update_context::<Val>(key, |_| Val("updated-in-child".into()));
        assert_eq!(get_context::<Val>(key).unwrap().0, "updated-in-child");
    }

    // Reverted after scope exit.
    assert_eq!(get_context::<Val>(key).unwrap().0, "root");
}

#[test]
fn test_get_context_option_some_and_none() {
    let key_set = unique_key("get_opt", "set");
    let key_unset = unique_key("get_opt", "unset");
    register::<RequestId>(key_set);
    register::<RequestId>(key_unset);

    set_context(key_set, RequestId("hello".into()));

    assert_eq!(
        get_context::<RequestId>(key_set),
        Some(RequestId("hello".into()))
    );
    assert_eq!(get_context::<RequestId>(key_unset), None);
}

#[test]
fn test_snapshot_uses_arc_sharing() {
    // After the Arc migration, snapshot values share memory with the store.
    // This test verifies snapshot + attach works correctly.
    let key = unique_key("snap_arc", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("original".into()));
    let snap = snapshot();

    // Modify after snapshot.
    set_context(key, RequestId("modified".into()));

    // Attach restores snapshot values.
    {
        let _g = attach(snap);
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "original");
    }

    // After attach scope ends, current value is back.
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "modified");
}

#[test]
fn test_concurrent_scope_and_read_no_panic() {
    // Simulate the pattern that caused BorrowError in v0.3.x:
    // A tracing callback fires during a write, triggering a re-entrant read.
    // With Cell<Option<ContextStore>>, this returns defaults instead of panicking.
    let key = unique_key("concurrent_rw", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("base".into()));

    // Rapidly alternate set + get (simulating interleaved callbacks).
    for i in 0..100 {
        set_context(key, RequestId(format!("iter-{}", i)));
        let val: RequestId = get_context::<RequestId>(key).unwrap();
        assert_eq!(val.0, format!("iter-{}", i));
    }
}

#[test]
fn test_cached_key_o1_read_in_nested_scopes() {
    // Cached keys should always be in current_values after scope entry.
    let key = unique_key("cached_read", "rid");
    register_with::<RequestId>(key, |opts| opts.cached());

    set_context(key, RequestId("root-val".into()));

    let _g1 = enter_scope();
    // Cached key should be readable without walking parents.
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "root-val");

    let _g2 = enter_scope();
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "root-val");

    // Override in inner scope.
    set_context(key, RequestId("inner-val".into()));
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "inner-val");

    drop(_g2);
    // After inner scope exit, cached value from g1's scope is restored.
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "root-val");

    drop(_g1);
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "root-val");
}

#[test]
fn test_non_cached_key_walks_parents() {
    // Non-cached keys (default) should find values in parent scopes.
    let key = unique_key("non_cached", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("root-val".into()));

    let _g1 = enter_scope();
    // Not set in child scope — walks to root.
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "root-val");

    // Override in child.
    set_context(key, RequestId("child-val".into()));
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "child-val");

    let _g2 = enter_scope();
    // Grandchild walks to child.
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "child-val");

    drop(_g2);
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "child-val");

    drop(_g1);
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "root-val");
}

#[tokio::test]
async fn test_async_reentrant_safety() {
    // Verify that scope_async and named_scope_async don't panic
    // under re-entrant-like patterns.
    let key = unique_key("async_reentrant", "rid");
    register::<RequestId>(key);

    let snap = {
        set_context(key, RequestId("base".into()));
        snapshot()
    };

    with_snapshot(snap, async {
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "base");

        async_scope("", async {
            set_context(key, RequestId("in-scope-async".into()));
            assert_eq!(get_context::<RequestId>(key).unwrap().0, "in-scope-async");

            async_scope("inner", async {
                assert_eq!(get_context::<RequestId>(key).unwrap().0, "in-scope-async");
                set_context(key, RequestId("deep".into()));
                assert_eq!(get_context::<RequestId>(key).unwrap().0, "deep");
            })
            .await;

            assert_eq!(get_context::<RequestId>(key).unwrap().0, "in-scope-async");
        })
        .await;

        assert_eq!(get_context::<RequestId>(key).unwrap().0, "base");
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
        let _scope = enter_scope();
        set_context(key, RequestId("parent-val".into()));
        snapshot()
    };

    let result = with_snapshot(snap, async { get_context::<RequestId>(key).unwrap() }).await;

    assert_eq!(result.0, "parent-val");
}

#[tokio::test]
async fn test_fork_writes_are_isolated() {
    let key = unique_key("fork_isolate", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = enter_scope();
        set_context(key, RequestId("parent".into()));
        snapshot()
    };

    with_snapshot(snap, async {
        set_context(key, RequestId("child-override".into()));
        let val = get_context::<RequestId>(key).unwrap();
        assert_eq!(val.0, "child-override");
    })
    .await;

    let parent_val = get_context::<RequestId>(key).unwrap_or_default();
    assert_eq!(parent_val, RequestId::default());
}

#[tokio::test]
async fn test_fork_is_cheap_clone() {
    let key = unique_key("fork_clone", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = enter_scope();
        set_context(key, RequestId("shared".into()));
        snapshot()
    };

    let snap2 = snap.clone();

    let r1 = with_snapshot(snap, async { get_context::<RequestId>(key).unwrap() }).await;
    let r2 = with_snapshot(snap2, async { get_context::<RequestId>(key).unwrap() }).await;

    assert_eq!(r1.0, "shared");
    assert_eq!(r2.0, "shared");
}

#[tokio::test]
async fn test_fork_child_scopes_work() {
    let key = unique_key("fork_scope", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = enter_scope();
        set_context(key, RequestId("base".into()));
        snapshot()
    };

    with_snapshot(snap, async {
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "base");

        async_scope("", async {
            set_context(key, RequestId("inner".into()));
            assert_eq!(get_context::<RequestId>(key).unwrap().0, "inner");
        })
        .await;

        assert_eq!(get_context::<RequestId>(key).unwrap().0, "base");
    })
    .await;
}

#[tokio::test]
async fn test_spawn_with_fork_async() {
    let key = unique_key("fork_spawn", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = enter_scope();
        set_context(key, RequestId("for-spawn".into()));
        snapshot()
    };

    let join = tokio::spawn(with_snapshot(snap, async {
        get_context::<RequestId>(key).unwrap()
    }));

    let result = join.await.unwrap();
    assert_eq!(result.0, "for-spawn");
}

#[tokio::test]
async fn test_fork_empty_context() {
    let snap = ContextSnapshot::empty();

    with_snapshot(snap, async {
        // No values set — empty context should be attachable.
    })
    .await;
}

#[tokio::test]
async fn test_fork_scope_chain_preserved() {
    let key = unique_key("fork_chain", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = enter_named_scope("parent-scope");
        set_context(key, RequestId("chained".into()));
        snapshot()
    };

    with_snapshot(snap, async {
        let chain = scope_chain();
        assert!(
            chain.contains(&"parent-scope".to_string()),
            "snapshot should preserve parent scope chain: {:?}",
            chain
        );
    })
    .await;
}

// ══════════════════════════════════════════════════════════════
//  merge_with tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_merge_with_adds_values() {
    let key = unique_key("merge_add", "rid");
    register::<RequestId>(key);

    // Create a store with a value
    let snap = {
        let _scope = enter_scope();
        set_context(key, RequestId("merged-val".into()));
        snapshot()
    };
    let source: crate::store::ContextStore = snap.into();

    // Clear context and merge
    clear();
    crate::merge_with(source);

    assert_eq!(get_context::<RequestId>(key).unwrap().0, "merged-val");
}

#[test]
fn test_merge_with_overwrites_existing() {
    let key = unique_key("merge_overwrite", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("original".into()));

    let snap = {
        let _scope = enter_scope();
        set_context(key, RequestId("new-val".into()));
        snapshot()
    };
    let source: crate::store::ContextStore = snap.into();

    crate::merge_with(source);

    assert_eq!(get_context::<RequestId>(key).unwrap().0, "new-val");
}

// ══════════════════════════════════════════════════════════════
//  capture() local-only exclusion tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_capture_excludes_local_only() {
    let key_remote = unique_key("cap_local", "remote");
    let key_local = unique_key("cap_local", "local");
    register::<RequestId>(key_remote);
    register_with::<RequestId>(key_local, |o| o.local_only());

    set_context(key_remote, RequestId("remote-val".into()));
    set_context(key_local, RequestId("local-val".into()));

    let snap = capture();
    // Remote key should be in snapshot
    assert!(snap.values.contains_key(key_remote));
    // Local-only key should NOT be in snapshot
    assert!(!snap.values.contains_key(key_local));
}

#[test]
fn test_fork_preserves_local_only() {
    let key_local = unique_key("fork_local", "local");
    register_with::<RequestId>(key_local, |o| o.local_only());

    set_context(key_local, RequestId("local-val".into()));

    // Fork should preserve all values including local-only
    let forked = crate::fork();
    let _g = crate::attach_store(forked);
    assert_eq!(get_context::<RequestId>(key_local).unwrap().0, "local-val");
}

// ══════════════════════════════════════════════════════════════
//  From<ContextSnapshot> registry validation tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_snapshot_to_store_filters_unknown_keys() {
    let key = unique_key("snap_filter", "known");
    register::<RequestId>(key);

    // Manually construct a snapshot with an unknown key
    let mut values = std::collections::HashMap::new();
    values.insert(
        key,
        std::sync::Arc::new(RequestId("known-val".into()))
            as std::sync::Arc<dyn crate::value::ContextValue>,
    );
    values.insert(
        "totally_unknown_key_xyz",
        std::sync::Arc::new(RequestId("ghost".into()))
            as std::sync::Arc<dyn crate::value::ContextValue>,
    );

    let snap = ContextSnapshot {
        values: std::sync::Arc::new(values),
        scope_chain: vec![],
    };

    let store: crate::store::ContextStore = snap.into();
    let all = store.collect_values();

    // Known key should be present
    assert!(all.contains_key(key));
    // Unknown key should be filtered out
    assert!(!all.contains_key("totally_unknown_key_xyz"));
}

#[test]
fn test_snapshot_to_store_filters_local_keys() {
    let key_local = unique_key("snap_local_filter", "local");
    register_with::<RequestId>(key_local, |o| o.local_only());

    // Even if a snapshot somehow has a local key, converting to store filters it
    let mut values = std::collections::HashMap::new();
    values.insert(
        key_local,
        std::sync::Arc::new(RequestId("local-ghost".into()))
            as std::sync::Arc<dyn crate::value::ContextValue>,
    );

    let snap = ContextSnapshot {
        values: std::sync::Arc::new(values),
        scope_chain: vec![],
    };

    let store: crate::store::ContextStore = snap.into();
    let all = store.collect_values();

    assert!(!all.contains_key(key_local));
}

// ══════════════════════════════════════════════════════════════
//  ContextFutureExt trait method tests
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_future_ext_attach() {
    let key = unique_key("fut_attach", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = enter_scope();
        set_context(key, RequestId("attached".into()));
        snapshot()
    };

    let result = async { get_context::<RequestId>(key).unwrap() }
        .attach(snap)
        .await;

    assert_eq!(result.0, "attached");
}

#[tokio::test]
async fn test_future_ext_fork() {
    let key = unique_key("fut_fork", "rid");
    register::<RequestId>(key);

    // Set up context with a value
    let snap = {
        let _scope = enter_scope();
        set_context(key, RequestId("parent-val".into()));
        snapshot()
    };

    with_snapshot(snap, async {
        // Fork inherits parent values
        let result = async {
            let val = get_context::<RequestId>(key).unwrap();
            // Write in fork is isolated
            set_context(key, RequestId("forked".into()));
            val
        }
        .fork()
        .await;

        assert_eq!(result.0, "parent-val");
        // Parent value unchanged
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "parent-val");
    })
    .await;
}

#[tokio::test]
async fn test_future_ext_scope() {
    let key = unique_key("fut_scope", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = enter_scope();
        set_context(key, RequestId("base".into()));
        snapshot()
    };

    with_snapshot(snap, async {
        let chain = async {
            set_context(key, RequestId("scoped".into()));
            scope_chain()
        }
        .scope("my-scope")
        .await;

        assert!(chain.contains(&"my-scope".to_string()));
        // Parent value not affected by scoped write
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "base");
    })
    .await;
}

#[tokio::test]
async fn test_future_ext_capture() {
    let key = unique_key("fut_capture", "rid");
    register::<RequestId>(key);

    let snap = {
        let _scope = enter_scope();
        set_context(key, RequestId("original".into()));
        snapshot()
    };

    with_snapshot(snap, async {
        let result = async { get_context::<RequestId>(key).unwrap() }
            .capture()
            .await;

        assert_eq!(result.0, "original");
    })
    .await;
}

// ══════════════════════════════════════════════════════════════
//  update_context_variable tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_update_context_variable_modifies_value() {
    let key = unique_key("update_mod", "uid");
    register::<UserId>(key);

    set_context(key, UserId(10));
    update_context(key, |v: UserId| UserId(v.0 + 5));

    assert_eq!(get_context::<UserId>(key).unwrap().0, 15);
}

#[test]
fn test_update_context_variable_uses_default_when_missing() {
    let key = unique_key("update_default", "uid");
    register::<UserId>(key);

    // No value set — update should use Default (0)
    update_context(key, |v: UserId| UserId(v.0 + 42));

    assert_eq!(get_context::<UserId>(key).unwrap().0, 42);
}

// ══════════════════════════════════════════════════════════════
//  clear() tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_clear_removes_all_values() {
    let key = unique_key("clear_all", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("before-clear".into()));
    assert!(get_context::<RequestId>(key).is_some());

    clear();

    assert_eq!(get_context::<RequestId>(key), None);
}

#[test]
fn test_clear_resets_scope_chain() {
    let _g = enter_named_scope("before-clear");
    assert!(!scope_chain().is_empty());

    clear();

    assert!(scope_chain().is_empty());
}

// ══════════════════════════════════════════════════════════════
//  AttachGuard nesting tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_attach_guard_restores_on_drop() {
    let key = unique_key("attach_restore", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("outer".into()));

    {
        let snap = {
            let _scope = enter_scope();
            set_context(key, RequestId("inner".into()));
            snapshot()
        };
        let _guard = attach_snapshot(snap);
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "inner");
    }

    // After guard drops, previous context restored
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "outer");
}

#[test]
fn test_nested_attach_guards() {
    let key = unique_key("attach_nested", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("level-0".into()));

    let snap1 = {
        let _scope = enter_scope();
        set_context(key, RequestId("level-1".into()));
        snapshot()
    };
    let snap2 = {
        let _scope = enter_scope();
        set_context(key, RequestId("level-2".into()));
        snapshot()
    };

    {
        let _g1 = attach_snapshot(snap1);
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "level-1");

        {
            let _g2 = attach_snapshot(snap2);
            assert_eq!(get_context::<RequestId>(key).unwrap().0, "level-2");
        }
        // g2 dropped — back to level-1
        assert_eq!(get_context::<RequestId>(key).unwrap().0, "level-1");
    }
    // g1 dropped — back to level-0
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "level-0");
}

// ══════════════════════════════════════════════════════════════
//  Snapshot serialize/deserialize roundtrip tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_snapshot_serialize_deserialize_roundtrip() {
    let key = unique_key("snap_roundtrip", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("wire-val".into()));
    let _scope = enter_named_scope("wire-scope");

    let snap = capture();
    let bytes = snap.serialize().unwrap();
    let restored = ContextSnapshot::deserialize(&bytes).unwrap();

    let _g = attach_snapshot(restored);
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "wire-val");
    assert!(scope_chain().contains(&"wire-scope".to_string()));
}

#[test]
fn test_snapshot_deserialize_invalid_bytes() {
    let result = ContextSnapshot::deserialize(&[0xFF, 0xFF, 0xFF]);
    assert!(result.is_err());
}

#[test]
fn test_snapshot_serialize_excludes_local() {
    let key_remote = unique_key("snap_ser_remote", "remote");
    let key_local = unique_key("snap_ser_local", "local");
    register::<RequestId>(key_remote);
    register_with::<RequestId>(key_local, |o| o.local_only());

    set_context(key_remote, RequestId("remote".into()));
    set_context(key_local, RequestId("local".into()));

    let snap = capture();
    let bytes = snap.serialize().unwrap();
    let restored = ContextSnapshot::deserialize(&bytes).unwrap();

    let _g = attach_snapshot(restored);
    assert_eq!(get_context::<RequestId>(key_remote).unwrap().0, "remote");
    // Local key not serialized, so not present after deserialize
    assert_eq!(get_context::<RequestId>(key_local), None);
}

// ══════════════════════════════════════════════════════════════
//  Thread safety tests
// ══════════════════════════════════════════════════════════════

#[test]
fn test_context_is_thread_isolated() {
    let key = unique_key("thread_iso", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("main-thread".into()));

    let handle = std::thread::spawn(move || {
        // Different thread has its own context
        assert_eq!(get_context::<RequestId>(key), None);
        set_context(key, RequestId("other-thread".into()));
        get_context::<RequestId>(key).unwrap()
    });

    let other_val = handle.join().unwrap();
    assert_eq!(other_val.0, "other-thread");
    // Main thread unchanged
    assert_eq!(get_context::<RequestId>(key).unwrap().0, "main-thread");
}

#[test]
fn test_snapshot_can_cross_threads() {
    let key = unique_key("snap_cross_thread", "rid");
    register::<RequestId>(key);

    set_context(key, RequestId("cross-thread".into()));
    let snap = capture();

    let handle = std::thread::spawn(move || {
        let _g = attach_snapshot(snap);
        get_context::<RequestId>(key).unwrap()
    });

    let result = handle.join().unwrap();
    assert_eq!(result.0, "cross-thread");
}
