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
14. [Macros](#14-macros)
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
dcontext = "0.8"
```

Define a context type, register it once at startup, and then pick the module
that matches your execution model:

```rust
use dcontext::{initialize, async_ctx, sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[tokio::main]
async fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    initialize(builder);

    let snap = {
        let _guard = sync_ctx::enter_scope();
        sync_ctx::set_context("request_id", RequestId("req-123".into()));

        let rid = sync_ctx::get_context::<RequestId>("request_id").unwrap();
        assert_eq!(rid.0, "req-123");

        sync_ctx::snapshot()
    };

    async_ctx::with_context(snap, async {
        let rid = async_ctx::get_context::<RequestId>("request_id").unwrap();
        assert_eq!(rid.0, "req-123");
    })
    .await;
}
```

**Key principle:** register once at startup, then use `sync_ctx::*` in
thread-local code and `async_ctx::*` in Tokio task-local code. `get_context`
now returns `Option<T>`, so examples should `unwrap()`, `unwrap_or_default()`,
or otherwise handle missing values explicitly.

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

### Sync scopes with RAII guards

```rust
use dcontext::sync_ctx;

{
    let _guard = sync_ctx::enter_scope();
    sync_ctx::set_context("request_id", RequestId("child-scope".into()));
    // "child-scope" is visible here
}
// _guard dropped → value reverts to parent scope
```

### Replacing the removed `dcontext::scope(...)`

The old root-level closure helper was removed. Create an explicit guard instead:

```rust
use dcontext::sync_ctx;

{
    let _guard = sync_ctx::enter_scope();
    sync_ctx::set_context("request_id", RequestId("in-scope".into()));
    process_request();
}
// reverted here
```

### Nesting

Scopes nest naturally. Inner scopes shadow outer values:

```rust
use dcontext::sync_ctx;

let _outer = sync_ctx::enter_scope();
sync_ctx::set_context("user", UserId(1));

{
    let _inner = sync_ctx::enter_scope();
    sync_ctx::set_context("user", UserId(2));
    assert_eq!(sync_ctx::get_context::<UserId>("user").unwrap().0, 2);
}
assert_eq!(sync_ctx::get_context::<UserId>("user").unwrap().0, 1);
```

### Async scopes

For async code, use `async_ctx::scope` with Tokio task-local context:

```rust
use dcontext::async_ctx;

async_ctx::scope("request", async {
    async_ctx::set_context("request_id", RequestId("async-scope".into()));
    do_async_work().await;
})
.await;
// reverted after future completes
```

## 4. Reading and Writing Context

### Setting values

```rust
use dcontext::sync_ctx;

sync_ctx::set_context("request_id", RequestId("req-123".into()));
```

Values are written into the current topmost scope. If no scope is active, the
root scope is used.

### Getting values

```rust
use dcontext::sync_ctx;

let rid = sync_ctx::get_context::<RequestId>("request_id");
```

`get_context` returns `Option<T>` in both `sync_ctx` and `async_ctx`:

- `Some(value)` if the key currently has a value
- `None` if the key is unset in the active context

Use whichever handling style fits the call site:

```rust
let rid = sync_ctx::get_context::<RequestId>("request_id").unwrap();
let maybe_user = sync_ctx::get_context::<UserId>("user_id");
let counter = sync_ctx::get_context::<Counter>("counter").unwrap_or_default();
```

### Updating values (read-modify-write)

Use the module-specific `update_context` helper to read the current value,
transform it, and write it back:

```rust
use dcontext::sync_ctx;

sync_ctx::update_context::<Counter>("counter", |c| Counter(c.0 + 1));
```

There is an equivalent `async_ctx::update_context` for task-local code.

## 5. Snapshots and Cross-Thread Propagation

A `ContextSnapshot` is an immutable, `Clone + Send + Sync` capture of the
current context. Use it to propagate context across thread boundaries or to
bridge sync code into async code.

### Capture and restore

```rust
use dcontext::sync_ctx;

let _guard = sync_ctx::enter_scope();
sync_ctx::set_context("request_id", RequestId("req-123".into()));

let snap = sync_ctx::snapshot();

let _restore = sync_ctx::attach(snap);
let rid = sync_ctx::get_context::<RequestId>("request_id").unwrap();
assert_eq!(rid.0, "req-123");
```

### Context inheritance modes

When propagating context across boundaries, `dcontext` supports two capture
modes via [`ContextInheritance`]:

| Mode | Cost | Semantics |
|------|------|-----------|
| **Fork** (default) | O(current scope keys) | Child gets a frozen Arc reference to the parent's scope chain. Reads fall through; writes are copy-on-write isolated. |
| **Snapshot** | O(depth × keys) | Full deep copy of all effective values into a flat HashMap. Self-contained, no references back to parent. |

**When to use each:**

- **Fork** — best for parent-child task/thread relationships where the parent
  outlives the child. Cheap to create, shares values via Arc.
- **Snapshot** — best for cross-thread message passing, queuing, or any case
  where the context must be self-contained and the sender's context may change
  or disappear before the receiver uses it.

### Spawn helpers (capture + spawn)

These helpers capture context and immediately spawn a thread or task:

```rust
use dcontext::{ContextInheritance, spawn_with_sync_context, spawn_with_async_context,
               spawn_blocking_with_async_context};

// Sync → async task (e.g., from a sync handler into a Tokio task)
spawn_with_sync_context(ContextInheritance::Fork, async {
    let rid = dcontext::async_ctx::get_context::<RequestId>("request_id").unwrap();
});

// Async → async child task
spawn_with_async_context(ContextInheritance::Fork, async {
    let rid = dcontext::async_ctx::get_context::<RequestId>("request_id").unwrap();
});

// Async → blocking thread
spawn_blocking_with_async_context(ContextInheritance::Fork, || {
    let rid = dcontext::sync_ctx::get_context::<RequestId>("request_id").unwrap();
});
```

### Wrap helpers (capture now, spawn later)

When you need to capture context but **not** spawn immediately — e.g., for
task queues, retry wrappers, rate limiters, or priority queues — use the
wrap helpers:

```rust
use dcontext::{ContextInheritance, wrap_with_sync_context, wrap_with_async_context};

// Capture sync context into a future (does NOT spawn)
let wrapped = wrap_with_sync_context(ContextInheritance::Fork, async {
    dcontext::async_ctx::get_context::<RequestId>("request_id").unwrap()
});

// Enqueue for later execution
channel.send(Box::pin(wrapped)).await;

// Consumer spawns when ready
let task = channel.recv().await.unwrap();
tokio::spawn(task);
```

These decompose the spawn helpers into capture + wrap, leaving spawn to the
caller. The wrapped future carries its own task-local context scope.

### Cross-thread message passing (use Snapshot)

When sending context via a message/channel to another thread (not spawning a
child), use **Snapshot**. The snapshot is self-contained — no references back
to the sender's store — so it remains valid even after the sender's context
changes or is dropped:

```rust
use dcontext::sync_ctx;

// Sender thread
sync_ctx::set_context("request_id", RequestId("req-001".into()));
let snap = sync_ctx::snapshot();  // self-contained deep copy
tx.send(Message { context: snap, payload: data }).unwrap();

// Receiver thread (different thread)
let msg = rx.recv().unwrap();
let _guard = sync_ctx::attach(msg.context);  // install into receiver's store
let rid = sync_ctx::get_context::<RequestId>("request_id").unwrap();
```

**Do not use Fork for message passing** — Fork creates an `Arc`-shared
reference to the sender's live store. If the sender's scope is popped before
the receiver reads the context, the forked child may see stale or missing
values.

### Direct wire deserialization (no store mutation)

When receiving wire bytes from an RPC or message queue, you can deserialize
directly into a `ContextSnapshot` without mutating any live store:

```rust
// Wire bytes arrive from a remote process
let snap = dcontext::deserialize_to_snapshot(&wire_bytes)
    .unwrap_or_default();

// Use the snapshot however you need:
let _guard = sync_ctx::attach(snap.clone());  // install in sync context
// or
async_ctx::with_context(snap, async { ... }).await;  // install in async context
```

This avoids the old pattern of `deserialize_context` → `snapshot()` → `drop(guard)`.

### Propagation summary

| Pattern | Recommended API |
|---------|----------------|
| Parent spawns child task (async → async) | `spawn_with_async_context(Fork, fut)` |
| Parent spawns child task (sync → async) | `spawn_with_sync_context(Fork, fut)` |
| Async task → blocking thread | `spawn_blocking_with_async_context(Fork, f)` |
| Capture now, spawn/execute later | `wrap_with_sync_context(Fork, fut)` or `wrap_with_async_context(Fork, fut)` |
| Send context via channel/message to another thread | `sync_ctx::snapshot()` → send `ContextSnapshot` |
| Receive wire bytes from remote process | `deserialize_to_snapshot(&bytes)` |
| HTTP middleware: wire bytes → async handler | `deserialize_to_snapshot(&bytes)` → `async_ctx::with_context(snap, fut)` |

## 6. Async Integration (Tokio)

`dcontext` is Tokio-only for async propagation. Use `dcontext::async_ctx` for
async/task-local context and `dcontext::sync_ctx` for synchronous/thread-local
context.

### Establishing task-local context

```rust
use dcontext::{async_ctx, sync_ctx};

#[tokio::main]
async fn main() {
    let snap = {
        let _guard = sync_ctx::enter_scope();
        sync_ctx::set_context("request_id", RequestId("req-001".into()));
        sync_ctx::snapshot()
    };

    async_ctx::with_context(snap, async {
        let rid = async_ctx::get_context::<RequestId>("request_id").unwrap();
        assert_eq!(rid.0, "req-001");

        async_ctx::scope("request", async {
            do_async_work().await;
        })
        .await;
    })
    .await;
}
```

### No `force_thread_local` shim in normal code

The old `force_thread_local` escape hatch is gone from the recommended API
surface. In the dual-context model there is no runtime dispatch to override —
use `sync_ctx::*` when you want thread-local behavior and `async_ctx::*` when
you want task-local behavior.

### Spawning async tasks with context

```rust
use dcontext::{async_ctx, spawn_with_context_async};

async_ctx::with_context(snap, async {
    let handle = spawn_with_context_async(async {
        let rid = async_ctx::get_context::<RequestId>("request_id").unwrap();
        assert_eq!(rid.0, "req-001");
    });

    handle.await.unwrap();
})
.await;
```

### Storage model

| Code path | Storage used |
|-----------|--------------|
| `async_ctx::*` | Tokio task-local |
| `sync_ctx::*` | Thread-local |
| `spawn_with_context_async(...)` | Captures context and re-establishes task-local state |

## 7. Cross-Process Serialization

Context can be serialized for transport across process boundaries such as RPC,
message queues, or HTTP metadata.

### Binary serialization

```rust
use dcontext::sync_ctx;

let _guard = sync_ctx::enter_scope();
sync_ctx::set_context("request_id", RequestId("req-123".into()));
let bytes = sync_ctx::serialize_context().unwrap();

let _guard = sync_ctx::deserialize_context(&bytes).unwrap();
let rid = sync_ctx::get_context::<RequestId>("request_id").unwrap();
assert_eq!(rid.0, "req-123");
```

### What gets serialized?

- **Included:** registered keys with values set in the current effective context
- **Excluded:** local-only entries
- **Also included:** the current scope chain (wire format v2)

### Unknown keys

Unknown keys in received bytes are skipped. A receiver only needs to register
the keys it cares about.

## 8. Local-Only Entries

For values that should propagate within a process (via snapshots) but never be
serialized for cross-process transport.

### Registration

```rust
builder.register_local::<DbConnection>("db_conn");
builder.register_with::<CacheHandle>("cache", |opts| opts.local_only());
```

### Usage

Set the value using the local-only setter, then read it from the appropriate
context module:

```rust
set_context_local("db_conn", db_pool.clone());
let db = dcontext::sync_ctx::get_context::<DbConnection>("db_conn").unwrap();
```

### Propagation behavior

| Operation | Local-only entries? |
|-----------|---|
| `sync_ctx::snapshot()` / `sync_ctx::attach()` | ✅ Included |
| `spawn_with_context()` | ✅ Included |
| `async_ctx::with_context()` | ✅ Included |
| `sync_ctx::serialize_context()` | ❌ Excluded |
| Cross-process transport | ❌ Not available |

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
and remote process boundaries. Named scopes form the chain; unnamed scopes do
not appear.

### Named scopes

Create named scopes with `sync_ctx::enter_named_scope` (sync) or
`async_ctx::scope` (async):

```rust
use dcontext::sync_ctx;

let _guard = sync_ctx::enter_named_scope("api-gateway");
{
    let _inner = sync_ctx::enter_named_scope("auth-check");
    let chain = sync_ctx::scope_chain();
    // chain == vec!["api-gateway", "auth-check"]
}
```

For async code:

```rust
use dcontext::async_ctx;

async_ctx::scope("process-order", async {
    do_async_work().await;
})
.await;
```

### Querying the chain

Use the module that owns the active context store:

```rust
let sync_chain = dcontext::sync_ctx::scope_chain();
let async_chain = dcontext::async_ctx::scope_chain();
```

### Cross-process propagation

When context is serialized, the current scope chain is included in the wire
bytes. On the receiving side, `sync_ctx::deserialize_context` restores the
remote chain as a prefix:

```rust
let _guard = dcontext::sync_ctx::enter_named_scope("service-a");
let bytes = dcontext::sync_ctx::serialize_context().unwrap();

let _guard = dcontext::sync_ctx::deserialize_context(&bytes).unwrap();
let _local = dcontext::sync_ctx::enter_named_scope("service-b");
let chain = dcontext::sync_ctx::scope_chain();
// chain == ["service-a", "service-b"]
```

### Configuring max chain length

```rust
use dcontext::{set_max_scope_chain_len, max_scope_chain_len};

set_max_scope_chain_len(32);
let current = max_scope_chain_len();
```

### Snapshots carry the scope chain

```rust
let snap = dcontext::sync_ctx::snapshot();
let chain: &[String] = snap.scope_chain();
```

### Integration with tracing spans

`SyncDcontextLayer` appends span names to `sync_ctx::scope_chain()`. For async
services, `AsyncDcontextLayer` appends to `async_ctx::scope_chain()`.

### Integration with dactor

`dcontext-dactor` restores inbound actor context into `async_ctx`, so actor
handlers should read the chain with `dcontext::async_ctx::scope_chain()`.

## 14. Macros

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

`with_scope!` was removed in the dual-context redesign. Replace it with an
explicit guard in `sync_ctx`:

```rust
let _guard = dcontext::sync_ctx::enter_scope();
dcontext::sync_ctx::set_context("request_id", RequestId("req-001".into()));
do_work();
```

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
dcontext = "0.8"
dcontext-dactor = "0.8"
```

```rust
use dcontext_dactor::{ContextInboundInterceptor, ContextOutboundInterceptor};

runtime.add_outbound_interceptor(Box::new(ContextOutboundInterceptor::default()));
runtime.add_inbound_interceptor(Box::new(ContextInboundInterceptor::default()));
```

### How it works

1. **Outbound interceptor** captures context when sending a message.
2. **Inbound interceptor** restores context before the handler runs.
3. Actor handlers then read values from `dcontext::async_ctx`.

### Handlers get context automatically

```rust
#[async_trait]
impl Handler<MyMessage> for MyActor {
    async fn handle(&mut self, msg: MyMessage, ctx: &mut ActorContext) -> () {
        let rid = dcontext::async_ctx::get_context::<RequestId>("request_id").unwrap();
        println!("request_id = {}", rid.0);
    }
}
```

### Remote transport

```rust
use dcontext_dactor::register_context_headers;
use dactor::HeaderRegistry;

let mut registry = HeaderRegistry::new();
register_context_headers(&mut registry);
```

### Error policy

```rust
use dcontext_dactor::{ContextInboundInterceptor, ErrorPolicy};

ContextInboundInterceptor::default();
ContextInboundInterceptor::new(ErrorPolicy::Reject);
```

## 17. Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `base64` | ✅ | Base64 string serialization helpers for text transports |
| `context-key` | ✅ | `ContextKey<T>` compile-time type-safe wrapper |
| `tokio` | ✅ | Enables `async_ctx` and Tokio task-local propagation |

### Minimal

```toml
dcontext = { version = "0.8", default-features = false }
```

### Default (recommended)

```toml
dcontext = "0.8"
```

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

Starting with v0.8, `dcontext` uses two explicit context stores:

- `dcontext::sync_ctx` — thread-local storage for synchronous code
- `dcontext::async_ctx` — Tokio task-local storage for asynchronous code

This removes the old root-level dispatch layer. Choose the module that matches
your execution model instead of relying on runtime detection.

### `async_ctx` — Task-Local Context

```rust
use dcontext::async_ctx;

let snap = dcontext::ContextSnapshot::empty();
async_ctx::with_context(snap, async {
    async_ctx::scope("handle_request", async {
        async_ctx::set_context("request_id", "req-123".to_string());
        let rid = async_ctx::get_context::<String>("request_id");
        assert_eq!(rid.as_deref(), Some("req-123"));
    })
    .await;

    assert!(async_ctx::scope_chain().is_empty());
})
.await;
```

### `sync_ctx` — Thread-Local Context

```rust
use dcontext::sync_ctx;

let _guard = sync_ctx::enter_named_scope("worker");
sync_ctx::set_context("task_id", "task-42".to_string());
let chain = sync_ctx::scope_chain();
assert_eq!(chain, vec!["worker"]);
```

### Bridging: Async → Sync

```rust
use dcontext::{async_ctx, sync_ctx};

let snap = async_ctx::snapshot();
tokio::task::spawn_blocking(move || {
    sync_ctx::restore(snap);
    do_blocking_work();
})
.await
.unwrap();
```

### Propagating to Child Tasks

```rust
use dcontext::{async_ctx, spawn_with_context_async};

async_ctx::with_context(dcontext::ContextSnapshot::empty(), async {
    let handle = spawn_with_context_async(async {
        do_child_work().await;
    });
    handle.await.unwrap();
})
.await;
```

### New Tracing Layers

| Layer | Store | Behavior |
|-------|-------|----------|
| `AsyncDcontextLayer` | Task-local | Persists across `.await` points |
| `SyncDcontextLayer` | Thread-local | Standard sync span lifecycle |
| `DcontextLayer` | Legacy alias | Kept only for compatibility |

### Migration from Pre-v0.8

| Old API | New API |
|---------|---------|
| `dcontext::set_context(val)` | `dcontext::sync_ctx::set_context(val)` |
| `dcontext::get_context::<T>()` | `dcontext::sync_ctx::get_context::<T>()` |
| `dcontext::enter_scope()` | `dcontext::sync_ctx::enter_scope()` |
| `dcontext::enter_named_scope(name)` | `dcontext::sync_ctx::enter_named_scope(name)` |
| `dcontext::scope(|| { ... })` | `let _guard = dcontext::sync_ctx::enter_scope();` |
| `dcontext::snapshot()` | `dcontext::sync_ctx::snapshot()` |
| `dcontext::attach(snap)` | `dcontext::sync_ctx::attach(snap)` |
| `dcontext::scope_chain()` | `dcontext::sync_ctx::scope_chain()` or `dcontext::async_ctx::scope_chain()` |
| `dcontext::serialize_context()` | `dcontext::sync_ctx::serialize_context()` |
| `dcontext::deserialize_context(bytes)` | `dcontext::sync_ctx::deserialize_context(bytes)` |
| `dcontext::with_context(snap, fut)` | `dcontext::async_ctx::with_context(snap, fut)` |
| `dcontext::scope_async(fut)` | `dcontext::async_ctx::scope("", fut)` |
| `dcontext::named_scope_async(name, fut)` | `dcontext::async_ctx::scope(name, fut)` |
| `with_scope! { ... }` | Removed; use `let _guard = dcontext::sync_ctx::enter_scope();` |
