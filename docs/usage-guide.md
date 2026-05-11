# dcontext Usage Guide

This guide covers how to use `dcontext` for distributed context propagation
in Rust applications — from basic scoping to cross-process serialization,
async integration, and actor framework support.

---

## Table of Contents

1. [Getting Started](#1-getting-started)
2. [Registration](#2-registration)
3. [Scopes and Rollback](#3-scopes-and-rollback)
4. [Reading and Writing Context](#4-reading-and-writing-context)
5. [Snapshots and Cross-Thread Propagation](#5-snapshots-and-cross-thread-propagation)
6. [Async Integration (Tokio)](#6-async-integration-tokio)
7. [Cross-Process Serialization](#7-cross-process-serialization)
8. [Local-Only Entries](#8-local-only-entries)
9. [Typed Keys (ContextKey)](#9-typed-keys-contextkey)
10. [Custom Codecs](#10-custom-codecs)
11. [Version Migration](#11-version-migration)
12. [Configuration](#12-configuration)
13. [Scope Chain](#13-scope-chain)
14. [Convenience Macros](#14-convenience-macros)
15. [Error Handling](#15-error-handling)
16. [Integration with dactor](#16-integration-with-dactor)
17. [Cargo Features](#17-cargo-features)
18. [Log Enrichment](#18-log-enrichment)
19. [Dual-Context Model (v0.8)](#19-dual-context-model-v08)

---

## 1. Getting Started

Add `dcontext` to your `Cargo.toml`:

```toml
[dependencies]
dcontext = "0.3"
```

Define a context type, register it, and use it:

```rust
use dcontext::{RegistryBuilder, initialize, enter_scope, get_context, set_context};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

fn main() {
    // 1. Register context types (once, at startup)
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    initialize(builder);

    // 2. Enter a scope and set values
    let _guard = enter_scope();
    set_context("request_id", RequestId("req-123".into()));

    // 3. Read values anywhere in the call stack
    let rid: RequestId = get_context("request_id");
    assert_eq!(rid.0, "req-123");

    // 4. When _guard drops, values revert to previous scope
}
```

**Key principle:** Register once at startup → set values in scopes → read
anywhere in the call stack. Scopes automatically revert on exit.

---

## 2. Registration

All context types must be registered before use. Registration happens once
at application startup via `RegistryBuilder`.

### Basic registration

```rust
let mut builder = RegistryBuilder::new();
builder.register::<RequestId>("request_id");
builder.register::<UserId>("user_id");
initialize(builder); // Freezes registry — all reads are lock-free
```

**Type constraints:** `T: Clone + Default + Send + Sync + Serialize + DeserializeOwned + 'static`

### Fallible registration

Use `try_register` and `try_initialize` when you need error handling
(e.g., in libraries or test setup where double-registration may happen):

```rust
let mut builder = RegistryBuilder::new();
builder.try_register::<RequestId>("request_id")?;
let _ = dcontext::try_initialize(builder); // Ok if already initialized
```

### Advanced registration

Use `register_with` for additional options:

```rust
builder.register_with::<TraceContext>("trace_ctx", |opts| {
    opts.version(2)            // Wire format version (default: 1)
        .codec(encode, decode) // Custom serialization codec
});
```

### Per-scope caching

By default, reading a value walks the parent scope chain (`O(depth)`).
For lightweight values that are read frequently (e.g. request IDs, trace IDs),
enable per-scope caching to get `O(1)` reads:

```rust
// Cached: effective value is Arc::cloned into each new scope on entry
builder.register_with::<RequestId>("request_id", |opts| opts.cached());

// Not cached (default): reads walk parent scopes, no copy on scope entry
builder.register::<LargePayload>("payload");
```

See [Custom Codecs](#11-custom-codecs) and [Version Migration](#12-version-migration)
for details.

---

## 3. Scopes and Rollback

Scopes are stack frames for context values. Each scope overlays the parent;
changes revert automatically when the scope exits.

### RAII guard

```rust
{
    let _guard = enter_scope();
    set_context("request_id", RequestId("child-scope".into()));
    // "child-scope" is visible here
}
// _guard dropped → value reverts to parent scope
```

### Closure-based scope

```rust
use dcontext::scope;

scope(|| {
    set_context("request_id", RequestId("in-scope".into()));
    process_request(); // sees "in-scope"
});
// reverted here
```

### Nesting

Scopes nest naturally. Inner scopes shadow outer values:

```rust
let _outer = enter_scope();
set_context("user", UserId(1));

{
    let _inner = enter_scope();
    set_context("user", UserId(2));
    assert_eq!(get_context::<UserId>("user").0, 2); // inner wins
}
assert_eq!(get_context::<UserId>("user").0, 1); // reverted to outer
```

### Async scope

For async code, use `async_ctx::scope` with Tokio task-local context:

```rust
use dcontext::async_ctx;

async_ctx::scope("request", async {
    async_ctx::set_context("request_id", RequestId("async-scope".into()));
    do_async_work().await;
}).await;
// reverted after future completes
```

---

## 4. Reading and Writing Context

### Setting values

```rust
set_context("request_id", RequestId("req-123".into()));
```

Values are set in the **current topmost scope**. If no scope is active,
the root scope is used.

### Getting values

```rust
let rid: RequestId = get_context("request_id");
```

Returns `T::default()` if the key is registered but no value has been set.
The type is inferred from the return type or can be specified with turbofish:

```rust
let rid = get_context::<RequestId>("request_id");
```

### Fallible variants

Use `try_get_context` and `try_set_context` when you need error handling:

```rust
match dcontext::try_get_context::<RequestId>("request_id") {
    Ok(Some(rid)) => println!("found: {:?}", rid),
    Ok(None)      => println!("registered but not set"),
    Err(e)        => println!("error: {}", e),
}
```

### Updating values (read-modify-write)

Use `update_context` to read the current value, transform it, and write back:

```rust
update_context::<Counter>("counter", |c| Counter(c.0 + 1));
```

This is a convenience over separate `get_context` + `set_context` calls. The
callback runs with the store fully available — re-entrant reads from tracing
callbacks work normally during the callback.

> **Note:** `update_context` is **not atomic** — another write may interleave
> between the read and the write. Last writer wins. This is by design for
> contention-free access.

---

## 5. Snapshots and Cross-Thread Propagation

A `ContextSnapshot` is an immutable, `Clone + Send + Sync` capture of the
current context. Use it to propagate context across thread boundaries.

### Capture and restore

```rust
use dcontext::{snapshot, attach};

let _guard = enter_scope();
set_context("request_id", RequestId("req-123".into()));

let snap = snapshot(); // capture current context

// Later, in another thread or scope:
let _restore = attach(snap); // push snapshot values as a new scope
let rid: RequestId = get_context("request_id"); // "req-123"
// _restore drops → reverts
```

### Spawning threads with context

```rust
use dcontext::spawn_with_context;

set_context("request_id", RequestId("req-001".into()));

let handle = spawn_with_context("worker-thread", || {
    let rid: RequestId = get_context("request_id");
    assert_eq!(rid.0, "req-001"); // inherited from parent
}).unwrap();

handle.join().unwrap();
```

### Wrapping closures

For thread pools or callbacks where you need a callable that carries context:

```rust
use dcontext::{wrap_with_context, wrap_with_context_fn};

// FnOnce — consumed on first call
let task = wrap_with_context(|| {
    let rid: RequestId = get_context("request_id");
    process(rid);
});
thread_pool.execute(task);

// Fn — reusable, context restored on each call
let handler = wrap_with_context_fn(|| {
    get_context::<RequestId>("request_id")
});
for _ in 0..10 {
    handler(); // same context each time
}
```

---

## 6. Async Integration (Tokio)

`dcontext` is Tokio-only for async context propagation. Use `dcontext::async_ctx`
for async/task-local context and `dcontext::sync_ctx` for
synchronous/thread-local context.

### Establishing task-local context

```rust
use dcontext::{async_ctx, sync_ctx};

#[tokio::main]
async fn main() {
    // Set up registry...

    // Build initial context in the sync store.
    let snap = {
        let _guard = sync_ctx::enter_scope();
        sync_ctx::set_context("request_id", RequestId("req-001".into()));
        sync_ctx::snapshot()
    };

    // Establish task-local context for async code.
    async_ctx::with_context(snap, async {
        let rid = async_ctx::get_context::<RequestId>("request_id").unwrap();
        assert_eq!(rid.0, "req-001");

        do_async_work().await; // context visible through .await points
    }).await;
}
```

### About `force_thread_local`

`force_thread_local` is a deprecated no-op compatibility shim kept for backward
compatibility. In the dual-context model there is no runtime dispatch to
override: `sync_ctx` is always thread-local and `async_ctx` is always Tokio
task-local. `force_thread_local(f)` simply calls `f()` and returns the result.

### Spawning async tasks with context

```rust
use dcontext::async_ctx;

async_ctx::with_context(snap, async {
    let child_snap = async_ctx::snapshot();

    let handle = tokio::spawn(async move {
        async_ctx::with_context(child_snap, async {
            let rid = async_ctx::get_context::<RequestId>("request_id").unwrap();
            // context inherited from parent task
        }).await;
    });

    handle.await.unwrap();
}).await;
```

### Storage model

| Code path | Storage used |
|-----------|--------------|
| `async_ctx::*` / `with_context(...)` | Tokio task-local |
| `sync_ctx::*` and sync helpers | Thread-local |
| `force_thread_local(...)` | Deprecated no-op shim; just runs `f()` |

---

## 7. Cross-Process Serialization

Context can be serialized to bytes for transport across process boundaries
(RPC, message queues, HTTP headers).

### Binary serialization

```rust
use dcontext::{serialize_context, deserialize_context};

// Sender side
let _guard = enter_scope();
set_context("request_id", RequestId("req-123".into()));
let bytes: Vec<u8> = serialize_context().unwrap();

// ... send bytes over the wire ...

// Receiver side
let _guard = deserialize_context(&bytes).unwrap();
let rid: RequestId = get_context("request_id");
assert_eq!(rid.0, "req-123");
// _guard drops → deserialized values revert
```

### Base64 string serialization

For HTTP headers, gRPC metadata, or environment variables (requires `base64`
feature, enabled by default):

```rust
use dcontext::{serialize_context_string, deserialize_context_string};

let encoded: String = serialize_context_string().unwrap();
// e.g., set as HTTP header: X-Context: <encoded>

// Receiver:
let _guard = deserialize_context_string(&encoded).unwrap();
```

### What gets serialized?

- **Included:** All registered keys with values set in the current context
- **Excluded:** Local-only entries (registered with `register_local` or set
  with `set_context_local`)
- **Format:** Bincode by default, or custom codec per key

### Unknown keys

Unknown keys in received bytes are **silently skipped**. This allows partial
receivers — a service only needs to register the keys it cares about.

---

## 8. Local-Only Entries

For values that should propagate within a process (via snapshots) but never
be serialized for cross-process transport.

### Registration

```rust
// Dedicated method (no Serialize/DeserializeOwned required)
builder.register_local::<DbConnection>("db_conn");

// Or via options
builder.register_with::<CacheHandle>("cache", |opts| opts.local_only());
```

**Type constraint:** `T: Clone + Default + Send + Sync + 'static` (no
`Serialize` needed)

### Usage

```rust
// Use set_context_local (not set_context) for local-only keys
set_context_local("db_conn", db_pool.clone());

// Reading works the same way
let db: DbConnection = get_context("db_conn");
```

### Propagation behavior

| Operation | Local-only entries? |
|-----------|---|
| `snapshot()` / `attach()` | ✅ Included |
| `spawn_with_context()` | ✅ Included |
| `with_context()` | ✅ Included |
| `serialize_context()` | ❌ Excluded |
| Cross-process transport | ❌ Not available |

---

## 9. Typed Keys (ContextKey)

The `context-key` feature (enabled by default) provides compile-time
type-safe access without string literals at call sites.

### Define keys as statics

```rust
use dcontext::ContextKey;

static REQUEST_ID: ContextKey<RequestId> = ContextKey::new("request_id");
static USER_ID: ContextKey<UserId> = ContextKey::new("user_id");
```

### Register and use

```rust
let mut builder = RegistryBuilder::new();
REQUEST_ID.register_on(&mut builder);
USER_ID.register_on(&mut builder);
initialize(builder);

// No string keys, no turbofish at call site
REQUEST_ID.set(RequestId("req-123".into()));
let rid = REQUEST_ID.get(); // type inferred from ContextKey<RequestId>
```

### Advantages

- **No typos** — key name is part of the static definition
- **No turbofish** — type is known from `ContextKey<T>`
- **Refactorable** — rename the static, refactoring tools handle it
- **Centralized** — all keys defined in one place

---

## 10. Custom Codecs

Replace the default bincode serialization with a custom format per key.

```rust
builder.register_with::<AppConfig>("config", |opts| {
    opts.codec(
        // Encoder: &T -> Result<Vec<u8>, String>
        |val: &AppConfig| serde_json::to_vec(val).map_err(|e| e.to_string()),
        // Decoder: &[u8] -> Result<T, String>
        |bytes: &[u8]| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
    )
});
```

### Use cases

- **Cross-language compatibility** — JSON or protobuf for polyglot systems
- **Debugging** — JSON is human-readable; bincode is not
- **Legacy compatibility** — custom wire format for existing systems

Both encoder and decoder must be provided together. The default codec is
bincode (fast, compact, Rust-native).

---

## 11. Version Migration

Support rolling upgrades where different nodes may run different schema
versions.

### Step 1: Version your type

```rust
// Current version (v2)
#[derive(Clone, Default, Serialize, Deserialize)]
struct TraceContext {
    trace_id: String,
    span_id: String,    // new in v2
    sampled: bool,      // new in v2
}

builder.register_with::<TraceContext>("trace_ctx", |opts| opts.version(2));
```

### Step 2: Register migration from old version

```rust
// Old version (v1) — only needed for deserialization
#[derive(Deserialize)]
struct TraceContextV1 {
    trace_id: String,
}

builder.register_migration::<TraceContextV1, TraceContext>(
    "trace_ctx",
    1, // from version 1
    |v1| TraceContext {
        trace_id: v1.trace_id,
        span_id: String::new(),  // default for new field
        sampled: true,           // default for new field
    },
);
```

### How it works

1. **Sender** serializes with its current version (version tag included in
   wire bytes)
2. **Receiver** looks up the version-specific deserializer
3. If versions differ, the migration function converts old → current
4. Unknown versions return `Err(DeserializationFailed)`

### Testing migrations

Use `make_wire_bytes` to construct wire bytes as if from an older sender:

```rust
use dcontext::make_wire_bytes;

let v1 = TraceContextV1 { trace_id: "abc".into() };
let v1_bytes = bincode::serialize(&v1).unwrap();
let wire = make_wire_bytes("trace_ctx", 1, &v1_bytes);

let _guard = deserialize_context(&wire).unwrap();
let tc: TraceContext = get_context("trace_ctx");
assert_eq!(tc.trace_id, "abc");
assert_eq!(tc.span_id, ""); // migrated default
```

---

## 12. Configuration

### Size limits

Prevent context from growing unbounded:

```rust
use dcontext::{set_max_context_size, serialize_context};

set_max_context_size(65_536); // 64 KB limit

// serialize_context() returns Err(ContextTooLarge) if exceeded
match serialize_context() {
    Ok(bytes) => send(bytes),
    Err(dcontext::ContextError::ContextTooLarge { size, limit }) => {
        eprintln!("context too large: {} > {} bytes", size, limit);
    }
    Err(e) => eprintln!("serialization error: {}", e),
}
```

Default is `0` (no limit). Set this early in application startup.

---

## 13. Scope Chain

The scope chain gives you a queryable call path that spans both local scopes
and remote process boundaries. Named scopes form the chain; unnamed scopes
(from plain `enter_scope()`) do not appear.

### Named scopes

Create named scopes with `enter_named_scope` (sync) or `named_scope_async`
(async):

```rust
use dcontext::{enter_named_scope, scope_chain};

let _guard = enter_named_scope("api-gateway");
{
    let _inner = enter_named_scope("auth-check");
    let chain = scope_chain();
    // chain == vec!["api-gateway", "auth-check"]
}
```

For async code, use `async_ctx::scope` with Tokio task-local context:

```rust
use dcontext::async_ctx;

async_ctx::scope("process-order", async {
    // scope chain includes "process-order" across .await points
    do_async_work().await;
}).await;
```

### Querying the chain

`scope_chain()` returns `Vec<String>` with the full chain — remote prefix
entries (from deserialized context) followed by local named scopes:

```rust
let chain: Vec<String> = dcontext::scope_chain();
// e.g. ["remote:service-a", "api-handler", "db-query"]
```

### Cross-process propagation

When context is serialized (wire format v2), the current scope chain is
included in the wire bytes. On the receiving side, `deserialize_context`
restores the remote chain as a prefix. New local named scopes are appended:

```rust
// Process A
let _guard = enter_named_scope("service-a");
let bytes = dcontext::serialize_context().unwrap();

// Process B (after deserialize_context)
let _guard = enter_named_scope("service-b");
let chain = scope_chain();
// chain == ["service-a", "service-b"]
```

Wire format v1 payloads (from older senders) are still readable — the scope
chain will simply be empty for the remote prefix.

### Configuring max chain length

Prevent unbounded chain growth in deep call graphs:

```rust
use dcontext::{set_max_scope_chain_len, max_scope_chain_len};

set_max_scope_chain_len(32); // cap at 32 entries

let current = max_scope_chain_len(); // read current limit
```

Default is `64`. Set to `0` for unlimited. When the limit is reached, new
named scopes still create context scopes, but their names are not appended
to the chain.

### Snapshots carry the scope chain

`ContextSnapshot` includes the scope chain at capture time. Access it via:

```rust
let snap = dcontext::snapshot();
let chain: &[String] = snap.scope_chain();
```

### Integration with tracing spans

When using `dcontext-tracing`, span names automatically become scope names.
Each span enter pushes the span name onto the chain:

```rust
let _span = tracing::info_span!("handle_request").entered();
let chain = dcontext::scope_chain();
// chain includes "handle_request"
```

> **Note:** `dcontext-tracing` uses `force_thread_local()`, so the chain
> reflects thread-local state. For full async propagation, combine with
> `dcontext::async_ctx::with_context()`.

### Integration with dactor

When using `dcontext-dactor`, the inbound interceptor's `wrap_handler`
automatically creates a named scope `remote:<actor_name>` for each inbound
message. This makes the distributed call boundary visible in the chain:

```rust
// Inside an actor handler for "OrderActor":
let chain = dcontext::scope_chain();
// e.g. ["api-gateway", "remote:OrderActor"]
```

---

## 14. Convenience Macros

### `register_contexts!`

Register multiple types at once:

```rust
use dcontext::{register_contexts, RegistryBuilder, initialize};

let mut builder = RegistryBuilder::new();
register_contexts!(builder, {
    "request_id" => RequestId,
    "user_id" => UserId,
    "trace_id" => TraceId,
});
initialize(builder);
```

### `with_scope!`

Enter a scope, set multiple values, and execute a block:

```rust
use dcontext::with_scope;

with_scope! {
    "request_id" => RequestId("req-001".into()),
    "user_id" => UserId(42),
    => {
        do_work(); // both values are set
    }
}
// both values auto-reverted
```

---

## 15. Error Handling

All errors are represented by `dcontext::ContextError`:

| Variant | When it occurs |
|---------|---------------|
| `NotRegistered` | Key not in registry |
| `AlreadyRegistered` | Key registered twice with different types |
| `TypeMismatch` | `get_context::<Wrong>()` type doesn't match registration |
| `SerializationFailed` | Codec encode error |
| `DeserializationFailed` | Codec decode error or unsupported version |
| `ContextTooLarge` | Serialized size exceeds `max_context_size` |
| `LocalOnlyKey` | Attempted to serialize a local-only entry |
| `NoActiveScope` | Reserved for future use |
| `RegistryFrozen` | `try_initialize()` called after registry already frozen |

**Panicking vs fallible APIs:**
- `get_context`, `set_context`, `initialize` — panic on error (ergonomic for
  well-known keys)
- `try_get_context`, `try_set_context`, `try_initialize` — return `Result`
  (defensive use in libraries)

---

## 16. Integration with dactor

The `dcontext-dactor` crate provides automatic context propagation through
[dactor](https://github.com/microsoft/dactor) actor messages.

### Setup

```toml
[dependencies]
dcontext = "0.3"
dcontext-dactor = "0.3"
```

```rust
use dcontext_dactor::{ContextOutboundInterceptor, ContextInboundInterceptor};

// Register interceptors on your runtime — that's it!
runtime.add_outbound_interceptor(Box::new(ContextOutboundInterceptor::default()));
runtime.add_inbound_interceptor(Box::new(ContextInboundInterceptor::default()));
```

### How it works

1. **Outbound interceptor** — captures context when sending a message
   - Local targets: snapshot (no serialization)
   - Remote targets: serialized wire bytes
2. **Inbound interceptor** — restores context automatically via `wrap_handler`
   - `on_receive`: normalizes wire bytes → snapshot
   - `wrap_handler`: wraps handler future with `dcontext::with_context`

### Handlers get context automatically

```rust
#[async_trait]
impl Handler<MyMessage> for MyActor {
    async fn handle(&mut self, msg: MyMessage, ctx: &mut ActorContext) -> () {
        // Context is automatically restored — no boilerplate needed
        let rid: RequestId = dcontext::get_context("request_id");
    }
}
```

### Remote transport

For cross-node propagation, register the wire header deserializer:

```rust
use dcontext_dactor::register_context_headers;
use dactor::HeaderRegistry;

let mut registry = HeaderRegistry::new();
register_context_headers(&mut registry);
// Pass registry to your transport
```

### Error policy

Configure behavior when serialization/deserialization fails:

```rust
use dcontext_dactor::{ContextInboundInterceptor, ErrorPolicy};

// Default: log warning, deliver message without context
ContextInboundInterceptor::default();

// Strict: reject messages with corrupt context
ContextInboundInterceptor::new(ErrorPolicy::Reject);
```

---

## 17. Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `base64` | ✅ | Base64 string serialization for HTTP headers |
| `context-key` | ✅ | `ContextKey<T>` compile-time type-safe wrapper |

Tokio async context support is built in. Use `async_ctx`, `with_context`, and
`spawn_with_context_async` inside a Tokio runtime.

### Minimal

```toml
dcontext = { version = "0.8", default-features = false }
```

### Default (recommended)

```toml
dcontext = "0.8"
```

---

## 18. Log Enrichment

The `dcontext-tracing` crate provides **log enrichment** — automatic injection
of context values into every log event. This is powered by the extensible
**per-key metadata** system in the core `dcontext` crate.

### 18.1 Extensible Metadata

Any typed metadata can be attached to a registration. Metadata is keyed by
`TypeId`, so different extension crates can each register their own metadata
type on the same key without conflicts:

```rust
use dcontext::RegistryBuilder;
use dcontext_tracing::TracingField;

let mut builder = RegistryBuilder::new();

// Register with TracingField metadata — both extract and enrich
builder.register_with::<String>("request_id", |opts| {
    opts.cached().with_metadata(
        TracingField::builder("rid")
            .extract_from_str(|s| Some(s.to_string()))
            .enrich_display::<String>()
            .build(),
    )
});

// Multiple metadata types on the same key (from different crates)
builder.register_with::<String>("tenant_id", |opts| {
    opts.cached()
        .with_metadata(
            TracingField::builder("tenant")
                .enrich_display::<String>()
                .build(),
        )
        // hypothetical: another crate's metadata
        // .with_metadata(PropagationHeader::new("X-Tenant-Id"))
});

// Keys without TracingField metadata are NOT included in logs
builder.register::<String>("internal_buffer");

dcontext::initialize(builder);
```

Each `with_metadata::<M>(value)` call stores under `TypeId::of::<M>()`.
Different metadata types coexist independently — one value per type per key.

### 18.2 TracingField Builder

`TracingField` is built with a fluent builder that controls three directions:

**Enrich — shorthand** (context → both log output AND span fields):

| Method | Formats via | Best for |
|--------|-------------|----------|
| `.enrich_display::<T>()` | `Display` trait | Strings, IDs, numbers |
| `.enrich_debug::<T>()` | `Debug` trait | Structs without Display |
| `.enrich_custom::<T>(f)` | Custom closure | Special formatting |

**Enrich log only** (context → log event, via `WithContextFields`):

| Method | Formats via |
|--------|-------------|
| `.enrich_log_display::<T>()` | `Display` trait |
| `.enrich_log_debug::<T>()` | `Debug` trait |
| `.enrich_log_custom::<T>(f)` | Custom closure |

**Record span only** (context → span field, on span enter):

| Method | Formats via |
|--------|-------------|
| `.enrich_span_display::<T>()` | `Display` trait |
| `.enrich_span_debug::<T>()` | `Debug` trait |
| `.enrich_span_custom::<T>(f)` | Custom closure |

> **Important:** Span recording only works on fields pre-declared with
> `tracing::field::Empty`. Spans without the field are silently skipped.

**Extract** (span field → context):

| Method | Converts from | Best for |
|--------|--------------|----------|
| `.extract_from_str(f)` | `&str` | String fields |
| `.extract_from_u64(f)` | `u64` | Unsigned integers |
| `.extract_from_i64(f)` | `i64` | Signed integers |
| `.extract_from_bool(f)` | `bool` | Boolean fields |

```rust
use dcontext_tracing::TracingField;

// All directions — extract from spans AND enrich both logs + span fields
TracingField::builder("rid")
    .extract_from_str(|s| Some(s.to_string()))
    .enrich_display::<String>()
    .build()

// Log enrichment only — no span recording
TracingField::builder("retry")
    .enrich_log_debug::<RetryCount>()
    .build()

// Span recording only — no log enrichment
TracingField::builder("job_id")
    .enrich_span_display::<String>()
    .build()

// Extract only — no enrichment at all
TracingField::builder("request_id")
    .extract_from_str(|s| Some(RequestId(s.to_string())))
    .build()

// Custom formatter with span field name override
TracingField::builder("uid")
    .record_as("user_id")  // span field name differs from log_name
    .enrich_custom::<UserId>(|u| format!("user:{}", u.0))
    .build()
```

The first argument to `builder()` is the `log_name` — the field name in log
output. Use `.span_field("other_name")` if the extraction span field name
differs from the context key, and `.record_as("field")` if the span field to
record into differs from `log_name`.

### 18.3 WithContextFields Formatter

`WithContextFields` wraps any `FormatEvent` implementation to prepend
context fields to every log line:

```rust
use tracing_subscriber::prelude::*;
use dcontext_tracing::{DcontextLayer, WithContextFields};

tracing_subscriber::registry()
    .with(DcontextLayer::new())
    .with(tracing_subscriber::fmt::layer()
        .event_format(WithContextFields::wrap(
            tracing_subscriber::fmt::format()
                .without_time()
                .with_target(false)
        )))
    .init();
```

Log output will look like:
```
rid=req-123 tenant=acme  INFO handling request
rid=req-123 tenant=acme  INFO query complete, rows=42
```

Only keys that have a `TracingField` with an enrich function **and** a value
set in the current context appear. Unset keys are silently skipped.

### 18.4 collect_log_fields()

For custom formatters or non-tracing logging, use `collect_log_fields()` to
get the current context fields as `(name, formatted_value)` pairs:

```rust
use dcontext_tracing::collect_log_fields;

let fields = collect_log_fields();
for (name, value) in &fields {
    print!("{}={} ", name, value);
}
```

### 18.5 Querying Metadata

The core `dcontext` crate exposes general-purpose metadata queries that any
extension crate can use:

```rust
use dcontext_tracing::TracingField;

// Query a single key's metadata (callback-based for thread safety)
if let Some(name) = dcontext::with_metadata::<TracingField, _>("request_id", |tf| tf.log_name()) {
    println!("Log field name: {}", name);
}

// Iterate all keys with a specific metadata type
let field_names: Vec<&str> = dcontext::keys_with_metadata::<TracingField, _>(|_key, tf| tf.log_name());
```

### 18.6 Writing Your Own Metadata

Extension crates define their own metadata struct and use the same registration API:

```rust
use dcontext_tracing::TracingField;

// In your crate
pub struct PropagationHeader {
    pub header_name: &'static str,
}

// Users register it alongside other metadata
builder.register_with::<String>("request_id", |opts| {
    opts.with_metadata(
            TracingField::builder("rid")
                .extract_from_str(|s| Some(s.to_string()))
                .enrich_display::<String>()
                .build(),
        )
        .with_metadata(PropagationHeader { header_name: "X-Request-Id" })
});

// Your crate queries only its own type
let headers = dcontext::keys_with_metadata::<PropagationHeader, _>(|key, ph| {
    (key, ph.header_name)
});
```

---

## 19. Dual-Context Model (v0.8)

Starting with v0.8, dcontext introduces a **dual-context model** that separates
async and sync context into independent stores. This eliminates a class of bugs
where `DcontextLayer` and application code (e.g., `scope_async`) sharing the
same thread-local store caused monotonic scope chain growth in production.

### The Problem (Pre-v0.8)

When `DcontextLayer` uses `force_thread_local` to push/pop scopes on span
enter/exit, and application code uses `scope_async` (which falls back to the
same thread-local when no task-local exists), depth mismatches cause scope
leaks on every yield in an instrumented async span.

### The Solution: Two Independent Modules

```rust
// Async code — operates on task-local store
use dcontext::async_ctx;

// Sync code — operates on thread-local store
use dcontext::sync_ctx;
```

### `async_ctx` — Task-Local Context

All functions in `async_ctx` access the **tokio task-local** store.
They no-op (or return empty results) if called outside an async task.

```rust
use dcontext::async_ctx;

// Establish a task-local context (typically done at request entry)
let snap = dcontext::ContextSnapshot::empty();
async_ctx::with_context(snap, async {
    // Push/pop scopes
    async_ctx::scope("handle_request", async {
        // Set/get values
        async_ctx::set_context("request_id", "req-123".to_string());
        let rid: Option<String> = async_ctx::get_context("request_id");

        // Nested scopes
        async_ctx::scope("process_item", async {
            tokio::task::yield_now().await; // safe — no leak
        }).await;
    }).await;

    // Scope reverted — scope chain is empty
    assert!(async_ctx::scope_chain().is_empty());
}).await;
```

### `sync_ctx` — Thread-Local Context

All functions in `sync_ctx` access the **thread-local** store.
They always succeed (thread-local is always available).

```rust
use dcontext::sync_ctx;

// Push/pop scopes
let _guard = sync_ctx::push_scope("worker");
sync_ctx::set_context("task_id", "task-42".to_string());
let chain = sync_ctx::scope_chain(); // vec!["worker"]

// Clear the thread-local context
sync_ctx::clear();
```

### Bridging: Async → Sync

Use snapshots to transfer context from async tasks to blocking threads:

```rust
use dcontext::{async_ctx, sync_ctx};

// In an async handler:
let snap = async_ctx::snapshot();
tokio::task::spawn_blocking(move || {
    sync_ctx::restore(snap); // one-way copy to thread-local
    // SyncDcontextLayer tracks spans here
    do_blocking_work();
}).await;
```

### Propagating to Child Tasks

```rust
use dcontext::async_ctx;

// In parent task:
let snap = async_ctx::snapshot();
tokio::spawn(async move {
    async_ctx::with_context(snap, async {
        // child task has parent's context
        do_child_work().await;
    }).await;
});
```

### New Tracing Layers

| Layer | Store | Behavior |
|-------|-------|----------|
| `AsyncDcontextLayer` | Task-local | Push on first enter, pop on close. Persists across yields. |
| `SyncDcontextLayer` | Thread-local | Push on enter, pop on exit. Standard sync lifecycle. |
| `DcontextLayer` (legacy) | Thread-local via `force_thread_local` | Preserved for backward compatibility. |

```rust
use tracing_subscriber::prelude::*;
use dcontext_tracing::{AsyncDcontextLayer, SyncDcontextLayer};

// For async services (recommended):
tracing_subscriber::registry()
    .with(AsyncDcontextLayer::new())
    .init();

// For sync-only code:
tracing_subscriber::registry()
    .with(SyncDcontextLayer::new())
    .init();
```

### Migration from Pre-v0.8

| Old API | New API |
|---------|---------|
| `scope_async(fut)` | `async_ctx::scope(name, fut)` |
| `scope_chain()` (ambiguous) | `async_ctx::scope_chain()` / `sync_ctx::scope_chain()` |
| `with_fork(handle, fut)` | `async_ctx::with_context(snapshot, fut)` |
| `fork()` | `async_ctx::snapshot()` |
| `DcontextLayer` | `AsyncDcontextLayer` + `SyncDcontextLayer` |
| `force_thread_local(f)` | Not needed — each module owns its store |

All old APIs remain available for backward compatibility.
