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
7. [Runtime-Agnostic Async](#7-runtime-agnostic-async)
8. [Cross-Process Serialization](#8-cross-process-serialization)
9. [Local-Only Entries](#9-local-only-entries)
10. [Typed Keys (ContextKey)](#10-typed-keys-contextkey)
11. [Custom Codecs](#11-custom-codecs)
12. [Version Migration](#12-version-migration)
13. [Configuration](#13-configuration)
14. [Scope Chain](#14-scope-chain)
15. [Convenience Macros](#15-convenience-macros)
16. [Error Handling](#16-error-handling)
17. [Integration with dactor](#17-integration-with-dactor)
18. [Cargo Features](#18-cargo-features)

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

For async code, use `scope_async` (requires `tokio` feature):

```rust
use dcontext::scope_async;

scope_async(async {
    set_context("request_id", RequestId("async-scope".into()));
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

When running inside a Tokio runtime, dcontext uses `tokio::task_local!`
for context storage. This requires explicit setup.

### Establishing task-local context

```rust
use dcontext::{with_context, snapshot, force_thread_local};

#[tokio::main]
async fn main() {
    // Set up registry...

    // Create initial context in thread-local (we're at top level)
    let snap = force_thread_local(|| {
        let _guard = dcontext::enter_scope();
        dcontext::set_context("request_id", RequestId("req-001".into()));
        dcontext::snapshot()
    });

    // Establish task-local context for async code
    with_context(snap, async {
        // All async code here sees the context
        let rid: RequestId = dcontext::get_context("request_id");
        assert_eq!(rid.0, "req-001");

        do_async_work().await; // context visible through .await points
    }).await;
}
```

### Why `force_thread_local`?

Inside a Tokio runtime, dcontext's storage dispatcher detects the runtime and
requires task-local context (to prevent accidental thread-local use that would
break across `.await` points). Use `force_thread_local` as an escape hatch
when you need to set up initial context before any task-local scope exists.

### Spawning async tasks with context

```rust
use dcontext::spawn_with_context_async;

with_context(snap, async {
    // Spawn a child task that inherits context
    let handle = spawn_with_context_async(async {
        let rid: RequestId = dcontext::get_context("request_id");
        // context inherited from parent task
    });
    handle.await.unwrap();
}).await;
```

### Storage dispatch rules

| Situation | Storage used |
|-----------|-------------|
| No Tokio runtime | Thread-local |
| Inside `with_context(...)` | Task-local |
| Inside Tokio but no `with_context` | **Panics** (safety check) |
| Inside `force_thread_local(...)` | Thread-local (forced) |

---

## 7. Runtime-Agnostic Async

For executors other than Tokio (async-std, smol, etc.), enable the
`context-future` feature:

```toml
[dependencies]
dcontext = { version = "0.3", features = ["context-future"] }
```

### ContextFuture

`ContextFuture` wraps any future with context propagation using thread-local
storage instead of task-local. It installs/removes context on each `poll()`.

```rust
use dcontext::with_context_future;

// Set up context in thread-local
force_thread_local(|| {
    let _guard = enter_scope();
    set_context("trace_id", TraceId("abc".into()));
});

let fut = with_context_future(async {
    // Context is available — no force_thread_local needed inside
    let tid: TraceId = get_context("trace_id");
    assert_eq!(tid.0, "abc");
});

// Run with any executor
smol::block_on(fut);
```

### Comparison with `with_context`

| | `with_context` (Tokio) | `with_context_future` (any runtime) |
|---|---|---|
| **Runtime** | Tokio only | Any executor |
| **Mechanism** | `tokio::task_local!` | Thread-local per `poll()` |
| **Overhead** | O(1) per poll | O(N) per poll (N = context keys) |
| **Feature** | `tokio` (default) | `context-future` |

Use `with_context` when you're on Tokio (zero overhead). Use
`with_context_future` when you need runtime portability.

---

## 8. Cross-Process Serialization

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

## 9. Local-Only Entries

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

## 10. Typed Keys (ContextKey)

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

## 11. Custom Codecs

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

## 12. Version Migration

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

## 13. Configuration

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

## 14. Scope Chain

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

For async code (requires `tokio` feature):

```rust
use dcontext::named_scope_async;

named_scope_async("process-order", async {
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
> `dcontext::with_context()`.

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

## 15. Convenience Macros

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

## 16. Error Handling

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

## 17. Integration with dactor

The `dcontext-dactor` crate provides automatic context propagation through
[dactor](https://github.com/Yaming-Hub/dactor) actor messages.

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

## 18. Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `tokio` | ✅ | Task-local storage, `with_context`, `spawn_with_context_async`, `scope_async` |
| `base64` | ✅ | Base64 string serialization for HTTP headers |
| `context-key` | ✅ | `ContextKey<T>` compile-time type-safe wrapper |
| `context-future` | ❌ | `ContextFuture` for non-Tokio executors |

### Minimal (no async)

```toml
dcontext = { version = "0.3", default-features = false }
```

### Full (all features)

```toml
dcontext = { version = "0.3", features = ["context-future"] }
```
