# dcontext Usage Guide

This guide covers how to use `dcontext` 0.9 for distributed context propagation in Rust applications — from basic scoping to async propagation, cross-process serialization, and registry configuration.

---

## Table of Contents

1. [Getting Started](#1-getting-started)
2. [Registration](#2-registration)
3. [Scopes and Rollback](#3-scopes-and-rollback)
4. [Reading and Writing Context](#4-reading-and-writing-context)
5. [ContextFutureExt Usage Patterns](#5-contextfutureext-usage-patterns)
6. [Fork vs Capture](#6-fork-vs-capture)
7. [Snapshots, Attach, and Merge](#7-snapshots-attach-and-merge)
8. [Cross-Process Serialization](#8-cross-process-serialization)
9. [Local-Only Variables](#9-local-only-variables)
10. [Typed Keys (ContextKey)](#10-typed-keys-contextkey)
11. [Custom Codecs](#11-custom-codecs)
12. [Version Migration](#12-version-migration)
13. [Configuration](#13-configuration)
14. [Scope Chain](#14-scope-chain)
15. [Macros](#15-macros)
16. [Error Handling](#16-error-handling)
17. [Integration with dactor](#17-integration-with-dactor)
18. [Cargo Features](#18-cargo-features)

---

## 1. Getting Started

Add `dcontext` to your `Cargo.toml`:

```toml
[dependencies]
dcontext = "0.9"
serde = { version = "1", features = ["derive"] }
```

Register your context types once at startup, then use the unified crate-root API everywhere:

```rust
use dcontext::{
    get_context_variable, initialize, push_scope, set_context_variable, RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
struct RequestId(String);

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    initialize(builder);

    let _scope = push_scope("request");
    set_context_variable("request_id", RequestId("req-123".into()));

    assert_eq!(
        get_context_variable::<RequestId>("request_id"),
        Some(RequestId("req-123".into()))
    );
}
```

**Key principle:** there is one active thread-local `ContextStore`. Async propagation works by wrapping futures, not by switching to a separate async API.

## 2. Registration

All context types must be registered before use.

### Basic registration

```rust
use dcontext::{initialize, RegistryBuilder};

let mut builder = RegistryBuilder::new();
builder.register::<String>("request_id");
builder.register::<u64>("user_id");
initialize(builder);
```

### Fallible startup

Use `try_initialize` if startup may run more than once:

```rust
use dcontext::{try_initialize, RegistryBuilder};

let mut builder = RegistryBuilder::new();
builder.register::<String>("request_id");
try_initialize(builder)?;
# Ok::<(), dcontext::ContextError>(())
```

### Advanced registration

Use `register_with` when a key needs custom behavior:

```rust
builder.register_with::<String>("request_id", |opts| opts.cached());
builder.register_with::<TraceContext>("trace", |opts| opts.version(2));
builder.register_with::<SessionState>("local_cache", |opts| opts.local_only());
```

### Registration options

`RegistrationOptions<T>` supports:

- `.version(version)`
- `.local_only()`
- `.cached()`
- `.codec(encode, decode)`
- `.with_metadata(metadata)`

### Migrations

Register older wire versions with `register_migration`:

```rust
builder.register_with::<TraceV2>("trace", |opts| opts.version(2));
builder.register_migration::<TraceV1, TraceV2>("trace", 1, |old| TraceV2 {
    trace_id: old.trace_id,
    span_id: "generated".into(),
});
```

## 3. Scopes and Rollback

Scopes are pushed with `push_scope(name)` and automatically reverted when the guard drops.

```rust
use dcontext::{get_context_variable, push_scope, set_context_variable};

let _outer = push_scope("request");
set_context_variable("user_id", 1_u64);

{
    let _inner = push_scope("db-call");
    set_context_variable("user_id", 2_u64);
    assert_eq!(get_context_variable::<u64>("user_id"), Some(2));
}

assert_eq!(get_context_variable::<u64>("user_id"), Some(1));
```

A scope only needs a name if you want it to appear in `scope_chain()`. Values still roll back correctly either way.

## 4. Reading and Writing Context

### Set a value

```rust
dcontext::set_context_variable("request_id", "req-123".to_string());
```

### Read a value

```rust
let rid = dcontext::get_context_variable::<String>("request_id");
```

`get_context_variable` returns `Option<T>`.

### Update in place

```rust
dcontext::update_context_variable::<u64>("attempts", |n| n + 1);
```

`update_context_variable` uses `T::default()` when the key is currently unset.

### Clear the active store

```rust
dcontext::clear();
```

## 5. ContextFutureExt Usage Patterns

`ContextFutureExt` is implemented for all `Sized` futures.

```rust
use dcontext::ContextFutureExt;
```

### `.with(store)`

Run a future with a specific `ContextStore`:

```rust
let store = dcontext::fork();
let fut = async move { /* ... */ }.with(store);
```

### `.attach(snapshot)`

Use a `ContextSnapshot` as the future's context:

```rust
let snap = dcontext::capture();
let fut = async move { /* ... */ }.attach(snap);
```

### `.fork()`

Spawn local work that should inherit the current context cheaply:

```rust
use dcontext::{ContextFutureExt, get_context_variable, set_context_variable};

set_context_variable("request_id", "req-123".to_string());

let handle = tokio::spawn(async move {
    assert_eq!(
        get_context_variable::<String>("request_id"),
        Some("req-123".to_string())
    );
}.fork());
```

### `.capture()`

Wrap a future with a snapshot-derived store:

```rust
let handle = tokio::spawn(async move {
    // sees a captured copy of the current context
}.capture());
```

### `.scope(name)`

Create a forked child store and immediately push a named scope:

```rust
let handle = tokio::spawn(async move {
    // scope_chain() includes "worker"
}.scope("worker"));
```

These adapters are runtime-agnostic: they work anywhere a future is polled.

## 6. Fork vs Capture

Both create a new context for downstream work, but they serve different goals.

| Operation | What it returns | Preserves local-only vars | Reads parent values | Intended use |
|-----------|------------------|---------------------------|---------------------|--------------|
| `fork()` | `ContextStore` | Yes | Yes, via frozen parent | local in-process propagation |
| `capture()` | `ContextSnapshot` | No | No live link; flattened snapshot | serialization, attach, cross-process |

### Prefer `fork()` for local spawn

```rust
let child_store = dcontext::fork();
tokio::spawn(async move {
    // child reads parent values, writes stay local
}.with(child_store));
```

### Prefer `capture()` for detached or portable context

```rust
let snap = dcontext::capture();
let bytes = snap.serialize()?;
# Ok::<(), dcontext::ContextError>(())
```

## 7. Snapshots, Attach, and Merge

### Attach an inbound snapshot

```rust
use dcontext::{attach_snapshot, ContextSnapshot};

fn handle(bytes: &[u8]) -> Result<(), dcontext::ContextError> {
    let snap = ContextSnapshot::deserialize(bytes)?;
    let _guard = attach_snapshot(snap);
    Ok(())
}
```

### Attach a store directly

```rust
let store = dcontext::fork();
let _guard = dcontext::attach_store(store);
```

### Merge values from another store

`merge_with` copies values into the current store without replacing the current scope chain:

```rust
let child = dcontext::fork();
dcontext::merge_with(child);
```

This is useful when a child computation returns updated context that should be folded back into the caller.

## 8. Cross-Process Serialization

### Outbound

```rust
let bytes = dcontext::capture().serialize()?;
# Ok::<(), dcontext::ContextError>(())
```

### Inbound

```rust
let snap = dcontext::ContextSnapshot::deserialize(&bytes)?;
let _guard = dcontext::attach_snapshot(snap);
# Ok::<(), dcontext::ContextError>(())
```

### Practical RPC pattern

Sender:

```rust
let outbound_bytes = dcontext::capture().serialize()?;
# Ok::<Vec<u8>, dcontext::ContextError>(outbound_bytes)
```

Receiver:

```rust
use dcontext::{attach_snapshot, ContextSnapshot};

fn handle_request(bytes: &[u8]) -> Result<(), dcontext::ContextError> {
    let snap = ContextSnapshot::deserialize(bytes)?;
    let _guard = attach_snapshot(snap);
    // request handling now sees the caller's distributed context
    Ok(())
}
```

`ContextSnapshot::serialize()` includes the captured scope chain and all serializable registered values.

## 9. Local-Only Variables

Mark a key local-only when it should stay inside the current process:

```rust
builder.register_with::<SessionCache>("session_cache", |opts| opts.local_only());
```

Local-only variables:

- are available through normal get/set APIs
- are preserved by `fork()` because the child stays in-process
- are excluded from `capture()`
- are excluded when a snapshot is restored into a `ContextStore`
- do not support wire versions or codecs because they are never serialized

This is the right choice for caches, handles, or non-serializable process-local state.

## 10. Typed Keys (ContextKey)

When the `context-key` feature is enabled, `ContextKey<T>` provides a typed wrapper around string keys.

```rust
use dcontext::ContextKey;

static REQUEST_ID: ContextKey<String> = ContextKey::new("request_id");

REQUEST_ID.set("req-123".to_string());
assert_eq!(REQUEST_ID.get(), Some("req-123".to_string()));
```

The underlying key still must be registered at startup.

## 11. Custom Codecs

Use a custom codec when a key should serialize with something other than bincode.

```rust
builder.register_with::<TraceContext>("trace", |opts| {
    opts.codec(
        |value| serde_json::to_vec(value).map_err(|e| e.to_string()),
        |bytes| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
    )
});
```

Custom codecs apply only to that key.

## 12. Version Migration

Migrations let new code read older wire payloads.

```rust
builder.register_with::<TraceV2>("trace", |opts| opts.version(2));
builder.register_migration::<TraceV1, TraceV2>("trace", 1, |old| TraceV2 {
    trace_id: old.trace_id,
    span_id: "migrated".into(),
});
```

Serialization always writes the key's current version. Deserialization accepts the current version plus any explicitly registered older versions.

## 13. Configuration

dcontext exposes runtime limits for serialized context size and visible scope-chain length:

```rust
dcontext::set_max_context_size(64 * 1024);
dcontext::set_max_scope_chain_len(32);
```

Query the active limits with:

- `max_context_size()`
- `max_scope_chain_len()`

## 14. Scope Chain

`scope_chain()` returns the visible scope names in order.

```rust
let _app = dcontext::push_scope("app");
let _handler = dcontext::push_scope("handler");
assert_eq!(dcontext::scope_chain(), vec!["app", "handler"]);
```

Serialized snapshots preserve the captured chain, so after `attach_snapshot` the inbound chain appears as the remote prefix.

## 15. Macros

Use `register_contexts!` to register many keys on a builder at once:

```rust
let mut builder = dcontext::RegistryBuilder::new();
dcontext::register_contexts!(builder, {
    "request_id" => String,
    "user_id" => u64,
    "trace_id" => String,
});
```

You still finish by calling `initialize(builder)` or `try_initialize(builder)`.

## 16. Error Handling

Most regular get/set operations are infallible at the API surface. Errors appear when working with initialization or wire formats:

- `try_initialize(builder)`
- `capture().serialize()`
- `ContextSnapshot::deserialize(bytes)`
- `register_migration(...)` / `register_with(...)` fallible variants

The main error type is `dcontext::ContextError`.

## 17. Integration with dactor

`dcontext` can be used with actor-style systems by treating each inbound message as a propagation boundary.

Typical pattern:

1. sender captures and serializes context
2. message carries the bytes
3. receiver deserializes and `attach_snapshot(...)`
4. handler code uses normal crate-root APIs
5. spawned actor work uses `.fork()` or `.scope(...)`

See `samples\src\bin\dactor_propagation.rs` for a concrete example.

## 18. Cargo Features

Current features:

- `base64` — enables base64 helpers used by tests/examples that encode serialized bytes as strings
- `context-key` — enables `ContextKey<T>`

Default features:

```toml
dcontext = { version = "0.9", default-features = true }
```

Disable defaults if you only want the core API:

```toml
dcontext = { version = "0.9", default-features = false }
```
