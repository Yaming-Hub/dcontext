# dcontext — Detailed Design

> **v0.9.0 Architecture — Unified Thread-Local Context**
>
> Starting with v0.9.0, dcontext uses a **single unified context model** based
> on one `thread_local!` store. The previous dual-context (`sync_ctx` / `async_ctx`)
> split and the Tokio dependency have been removed.
>
> Key architectural decisions:
>
> - **Single `thread_local! { Cell<Option<ContextStore>> }`** is the source of truth.
>   All sync code reads/writes it directly.
>
> - **`WithContext<F>` future wrapper** — swaps an owned `ContextStore` into/out
>   of thread-local storage on each `poll()`. This makes thread-local context
>   effectively task-local during a poll, without any runtime dependency.
>
> - **`ContextFutureExt` trait** — blanket-implemented for all `Sized` types,
>   provides `.with()`, `.fork()`, `.attach()`, `.capture()`, `.scope()` for
>   ergonomic async propagation.
>
> - **`Cell` take/set pattern** — no user code runs while the store is moved out.
>   Re-entrant access during the brief "busy" window sees `None` and returns
>   defaults gracefully — no panics.
>
> - **`Arc<dyn ContextValue>`** for all stored values — scope push/pop only bumps
>   reference counts. Values are shared cheaply across scope chain nodes and snapshots.
>
> - **Sparse scopes with parent-chain fallback** — each scope only stores keys
>   explicitly set in that scope. Reads walk the parent chain (`O(depth)`). Keys
>   registered with `.cached()` get `O(1)` reads.
>
> - **`ScopeNode`** (immutable, `Arc`-wrapped, `Send + Sync`) forms a persistent
>   linked list of frozen parent scopes.
>
> - **Registry DI** — internal functions accept `&Registry` for testability.
>   Production code resolves the global registry via `with_global_registry()`.
>
> - **No Tokio dependency** — the core crate is runtime-agnostic.
>
> - **`dcontext-tracing` removed** — tracing integration is no longer shipped.

## 1. Overview

`dcontext` is a **distributed context propagation** library for Rust. It provides a scoped, type-safe key–value store that travels with the execution flow — across function calls, async/sync boundaries, thread spawns, and even process boundaries via serialization.

### Motivation

In distributed applications, contextual information (request IDs, auth tokens, feature flags, trace baggage) often needs to flow through deep call stacks without threading every value through function parameters. Rust offers `thread_local!` and Tokio's `task_local!`, but these have gaps:

| Gap | Description |
|-----|-------------|
| **Sync ↔ Async boundary** | Spawning a blocking thread from an async task (or vice versa) loses task-local state. |
| **Thread spawns** | `std::thread::spawn` does not inherit the parent's thread-local values. |
| **Cross-process** | No built-in mechanism to serialize and restore context across RPC / message boundaries. |
| **Scoped rollback** | Neither `thread_local!` nor `task_local!` support a scope tree where leaving a scope reverts changes. |

`dcontext` fills all four gaps with a single propagation toolkit built around a
unified thread-local store and a future wrapper for async propagation.

### Design Principles

| Principle | Description |
|-----------|-------------|
| **Type-safe** | Values are stored as `Any` but retrieved with compile-time type checking via generics. |
| **Scoped** | Context modifications in a child scope are invisible after the scope exits. |
| **Portable** | Context can be serialized for cross-process propagation; the library handles the plumbing. |
| **Runtime-agnostic** | Async propagation uses a future wrapper (`WithContext`) — works with any executor. |
| **Zero-cost when unused** | Thread-local storage means no global locks on the hot path. ~4ns per poll overhead. |

---

## 2. Context Model

### 2.1 Conceptual Data Structure

At any point in time, the active context is a **stack of scopes**, where each scope is an overlay map:

```
Scope 2 (current) : { "trace_id" → TraceId("abc"), "flags" → Flags { debug: true } }
Scope 1           : { "flags" → Flags { debug: false }, "user" → User("alice") }
Scope 0 (root)    : { "trace_id" → TraceId("xyz") }
```

**Lookup** walks from the topmost scope downward and returns the first match.
In the example above, `get_context_variable::<TraceId>("trace_id")` returns `Some(TraceId("abc"))` (scope 2 shadows scope 0).

**Set** always writes to the topmost (current) scope, creating a shadow entry.

**Leave scope** drops scope 2, revealing the parent state — `trace_id` reverts to `"xyz"` and `flags` to `{ debug: false }`.

### 2.2 Type-Erased Storage

Each value in the map is stored as a **`ContextValue`** trait object:

```rust
pub trait ContextValue: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn serialize_value(&self) -> Result<Vec<u8>, ContextError>;
}
```

A blanket implementation covers all `T: Clone + Send + Sync + Serialize + DeserializeOwned + 'static`.

Values are wrapped in `Arc<dyn ContextValue>` — scope push/pop only bumps reference counts. No user `Clone` code runs during scope operations.

### 2.3 Context Registration

Before a context key can be used, its type must be **registered**. Registration records:

| Field | Type | Purpose |
|-------|------|---------|
| `key` | `&'static str` | Human-readable name, used as map key and serialization key |
| `type_id` | `TypeId` | Runtime type-safety validation |
| `key_version` | `u32` | Per-key wire format version |
| `deserializers` | `HashMap<u32, DeserializeFn>` | Versioned deserializer functions |
| `local_only` | `bool` | If true, excluded from serialization and capture |
| `cached` | `bool` | If true, effective value is eagerly copied on scope entry for O(1) reads |
| `serialize_fn` | `Option<SerializeFn>` | Custom serializer (overrides bincode) |
| `metadata` | `HashMap<TypeId, Box<dyn Any>>` | Extensible per-key metadata |

Registration is typically done once at startup:

```rust
let mut builder = dcontext::RegistryBuilder::new();
builder.register::<TraceContext>("trace_context");
builder.register::<FeatureFlags>("feature_flags");
dcontext::initialize(builder);
```

The registry uses a **two-phase model**:
- **Build phase**: `RegistryBuilder` collects registrations (no locks, not yet accessible).
- **Frozen phase**: `initialize(builder)` moves registrations into `OnceLock<RegistryMap>`. All subsequent reads are **lock-free**.

#### 2.3.1 Registration Options

Per-key options are configured with `RegistrationOptions<T>`:

```rust
builder.register_with::<RequestId>("request_id", |opts| {
    opts.cached()          // O(1) reads via eager copy on scope entry
        .version(2)        // Wire format version
        .local_only()      // Excluded from serialization
        .codec(enc, dec)   // Custom per-key serializer
        .with_metadata(m)  // Attach arbitrary typed metadata
});
```

#### 2.3.2 Typed Key Wrappers (Optional Ergonomic API)

For compile-time safety against key typos and type mismatches, the library
provides an optional `ContextKey<T>` newtype (feature: `context-key`):

```rust
static REQUEST_ID: ContextKey<RequestId> = ContextKey::new("request_id");

let rid = REQUEST_ID.get();     // returns Option<RequestId>
REQUEST_ID.set(new_value);
```

---

## 3. Storage Strategy

### 3.1 Single Thread-Local Store

The store lives in a `thread_local!` cell:

```rust
thread_local! {
    pub(crate) static CONTEXT: Cell<Option<ContextStore>> = Cell::new(Some(ContextStore::new()));
}
```

All public API functions operate on this single store via the `try_apply` helper:

```rust
pub(crate) fn try_apply<R>(f: impl FnOnce(&mut ContextStore) -> R) -> Option<R> {
    CONTEXT.try_with(|cell| {
        let mut store = cell.take()?;
        let result = f(&mut store);
        cell.set(Some(store));
        Some(result)
    }).ok().flatten()
}
```

### 3.2 `ContextStore` Structure

```rust
pub struct ContextStore {
    /// Frozen parent scope chain (immutable linked list of `Arc<ScopeNode>`)
    scope_chain: Option<Arc<ScopeNode>>,
    /// Active scope's values (sparse)
    current_values: HashMap<&'static str, Arc<dyn ContextValue>>,
    /// Name of the active scope
    current_name: Option<String>,
    /// Current scope depth
    depth: usize,
    /// Remote scope chain (from cross-process propagation)
    remote_chain: Arc<Vec<String>>,
    /// Frozen parent from a fork (read-through inheritance)
    frozen_parent: Option<Arc<ScopeNode>>,
    /// Scope barrier for deserialized contexts
    scope_barrier: Option<usize>,
}
```

`ContextStore` is `Send` — it can be moved across threads (e.g., into a spawned task via `WithContext`).

### 3.3 Scope Operations

| Operation | Effect |
|-----------|--------|
| `push_scope(name)` | Freezes current values into a `ScopeNode`, starts a new empty scope. For cached keys, copies effective values into the new scope. Returns `ScopeGuard`. |
| Drop `ScopeGuard` | Pops the scope, restoring the frozen parent `ScopeNode`. |
| `set_context_variable(key, val)` | Inserts/replaces in the current scope's values map. |
| `get_context_variable::<T>(key)` | Walks current values → scope chain → frozen parent. Returns first match. |

### 3.4 ScopeGuard

```rust
pub struct ScopeGuard {
    depth: usize,  // Expected depth for validation
    _not_send: PhantomData<*const ()>,  // !Send — must drop on same thread
}
```

Guards are `!Send` to prevent cross-thread scope corruption.

### 3.5 Maximum Scope Depth

A configurable `MAX_SCOPE_DEPTH` (default: 1024) prevents runaway recursion.
Beyond the limit, `push_scope` returns a no-op guard.

---

## 4. Async Propagation — `WithContext<F>`

### 4.1 Implementation

```rust
pin_project! {
    pub struct WithContext<F> {
        #[pin]
        inner: F,
        store: Option<ContextStore>,
    }
}

impl<F: Future> Future for WithContext<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let prev = CONTEXT.with(|cell| cell.replace(this.store.take()));
        let result = this.inner.poll(cx);
        *this.store = CONTEXT.with(|cell| cell.replace(prev));
        result
    }
}
```

On each poll:
1. Swap **our** store into thread-local (save previous)
2. Poll the inner future (which sees our store)
3. Swap back (our store may have been mutated, save it for next poll)

Overhead: ~4ns per poll (two `Cell` swaps).

### 4.2 `ContextFutureExt` Trait

Blanket-implemented for all `Sized` types:

```rust
pub trait ContextFutureExt: Sized {
    fn with(self, store: ContextStore) -> WithContext<Self>;
    fn attach(self, snap: ContextSnapshot) -> WithContext<Self>;
    fn fork(self) -> WithContext<Self>;
    fn capture(self) -> WithContext<Self>;
    fn scope(self, name: &str) -> WithContext<Self>;
}

impl<F: Sized> ContextFutureExt for F {}
```

| Method | Semantics |
|--------|-----------|
| `.with(store)` | Use a specific `ContextStore` |
| `.attach(snap)` | Convert snapshot to store and use it |
| `.fork()` | Fork current context (cheap, read-through parent) |
| `.capture()` | Snapshot current context (excludes local-only, full copy) |
| `.scope(name)` | Fork + push a named scope |

### 4.3 Send/Sync Properties

- `WithContext<F>` is `Send` when `F: Send` (store is Send)
- `ContextStore` is `Send` (values are `Arc<dyn ContextValue>` where `ContextValue: Send + Sync`)
- `ContextSnapshot` is `Send + Sync`
- `ScopeGuard` and `AttachGuard` are `!Send` (must drop on creating thread)

---

## 5. Public API

All operations live at the crate root (`dcontext::*`):

### 5.1 Scope Management

```rust
pub fn push_scope(name: &str) -> ScopeGuard;
pub fn scope_chain() -> Vec<String>;
```

### 5.2 Value Access

```rust
pub fn set_context_variable<T>(key: &'static str, value: T);
pub fn get_context_variable<T>(key: &str) -> Option<T>;
pub fn update_context_variable<T>(key: &'static str, f: impl FnOnce(T) -> T);
```

Type constraints: `T: Clone + Send + Sync + Serialize + DeserializeOwned + 'static`
(`get` only requires `T: Clone + Send + Sync + 'static`)

### 5.3 Propagation

```rust
pub fn capture() -> ContextSnapshot;       // Snapshot (excludes local-only)
pub fn fork() -> ContextStore;             // Child store (cheap, read-through)
pub fn attach_snapshot(snap: ContextSnapshot) -> AttachGuard;
pub fn attach_store(store: ContextStore) -> AttachGuard;
pub fn merge_with(source: ContextStore);   // Merge values into current store
pub fn clear();                            // Reset to empty
```

### 5.4 Registration and Initialization

```rust
pub fn initialize(builder: RegistryBuilder);
pub fn try_initialize(builder: RegistryBuilder) -> Result<(), ContextError>;
```

### 5.5 Configuration

```rust
pub fn set_max_context_size(limit: usize);
pub fn max_context_size() -> usize;
pub fn set_max_scope_chain_len(limit: usize);
pub fn max_scope_chain_len() -> usize;
```

### 5.6 Future Extensions (`ContextFutureExt`)

```rust
use dcontext::ContextFutureExt;

// Spawn with forked context (cheap, inherits reads)
tokio::spawn(async_work().fork());

// Spawn with captured context (full copy, excludes local-only)
tokio::spawn(async_work().capture());

// Attach inbound remote context
tokio::spawn(handler().attach(snap));

// Named scope for a future
tokio::spawn(handler().scope("request"));
```

---

## 6. Fork vs Capture

### 6.1 `fork()`

Creates a child `ContextStore` with a frozen parent:
- Reads **fall through** to the frozen parent chain (cheap, `Arc`-shared)
- Writes are **isolated** to the child (copy-on-write semantics)
- Local-only values **are preserved** (stays in-process)
- Operation is cheap: no value cloning, just an `Arc::clone` of the parent scope node

Use for: local task spawning where the child should see parent values but not modify them.

### 6.2 `capture()`

Creates a `ContextSnapshot`:
- Values are **flattened** (walked from scope chain into a single map)
- Local-only values are **excluded**
- The scope chain is preserved for display/restoration
- Result is `Send + Sync` and serializable

Use for: cross-process transfer, or when the child needs full independence.

### 6.3 `attach_snapshot()` / `attach_store()`

Both replace the active thread-local context entirely and return an `AttachGuard`.
When the guard drops, the previous context is restored.

Use at inbound boundaries (e.g., receiving a deserialized context from a remote service).

### 6.4 `merge_with()`

Copies values from another `ContextStore` into the current store without
replacing scope chain or existing values that the source doesn't have.

---

## 7. Serialization Wire Format

### 7.1 Format

The serialized context is a **bincode-encoded** envelope:

```rust
struct WireContext {
    version: u32,
    entries: Vec<WireEntry>,
}

struct WireEntry {
    key: String,
    key_version: u32,     // Per-key schema version
    value: Vec<u8>,       // Serialized value bytes (inner)
}
```

The outer container uses bincode; each value is independently serialized (also
bincode by default, overridable with custom codec). This two-level scheme
allows the receiver to skip unknown keys gracefully.

### 7.2 Serialization Workflow

**Outbound:**
```rust
let bytes = dcontext::capture().serialize()?;
// Send `bytes` over the wire
```

**Inbound:**
```rust
let snap = dcontext::ContextSnapshot::deserialize(&bytes)?;
let _guard = dcontext::attach_snapshot(snap);
// Context is now active
```

### 7.3 Version Compatibility

- `version = 1` is the initial wire format.
- Unknown keys in `entries` are silently ignored on deserialization.
- Per-key `key_version` mismatches invoke migration functions if registered, otherwise report `ContextError::DeserializationFailed`.
- Local-only keys are excluded from serialization and filtered on restoration.

### 7.4 Custom Codecs

Per-key custom encode/decode functions replace bincode for inner value serialization:

```rust
builder.register_with::<MyType>("my_key", |opts| {
    opts.codec(
        |val| serde_json::to_vec(val).map_err(|e| ...),
        |bytes| serde_json::from_slice(bytes).map_err(|e| ...),
    )
});
```

---

## 8. Local-Only Variables

Local-only is a **registration concern**, not a value-trait concern:

```rust
builder.register_with::<SecurityToken>("security_token", |opts| {
    opts.local_only()
});
```

Behavior:
- **Preserved** by `fork()` (stays in-process, linked via frozen parent)
- **Excluded** from `capture()` (never appears in snapshots)
- **Excluded** from serialized wire bytes
- **Filtered** when converting `ContextSnapshot` → `ContextStore` (defense against remote code with different local registrations)

---

## 9. Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("context key '{0}' is not registered")]
    NotRegistered(String),

    #[error("context key '{0}' is already registered with a different type")]
    AlreadyRegistered(String),

    #[error("type mismatch for key '{0}': expected {1}, got {2}")]
    TypeMismatch(String, String, String),

    #[error("serialization failed: {0}")]
    SerializationFailed(String),

    #[error("deserialization failed: {0}")]
    DeserializationFailed(String),

    #[error("no active scope: {0}")]
    NoActiveScope(String),

    #[error("context size exceeds limit: {size} bytes > {limit} bytes")]
    ContextTooLarge { size: usize, limit: usize },

    #[error("key '{0}' is local-only and cannot be serialized")]
    LocalOnlyKey(String),
}
```

---

## 10. Registry DI Pattern

Internal functions that depend on registrations accept `&Registry` instead of
reaching directly into global state:

```rust
pub(crate) struct Registry<'a> {
    map: &'a RegistryMap,
}

impl Registry<'_> {
    fn is_local_key(&self, key: &str) -> bool;
    fn is_valid_value(&self, key: &str, value: &dyn ContextValue) -> bool;
    fn cached_keys(&self) -> Vec<&'static str>;
    fn get_serialization_info(&self, key: &str) -> Option<SerializationInfo>;
}
```

Public functions call `with_global_registry(|reg| ...)` to resolve the frozen
or build-phase registry and pass it into internal logic. This keeps production
reads lock-free while letting tests build isolated `RegistryMap` instances and
exercise serialization, capture, filtering, and caching behavior without
mutating the global registry.

Examples:
- `wire::serialize_from(&Registry, values, scope_chain)`
- `wire::deserialize_to_snapshot(&Registry, bytes)`
- `ContextStore::push_scope(&Registry, name)`

---

## 11. Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `base64` | **yes** | Enables base64-oriented serialization helpers for text transports. |
| `context-key` | **yes** | Enables `ContextKey<T>` typed key wrapper for compile-time safe access. |

No Tokio dependency. The crate is runtime-agnostic.

---

## 12. Crate Structure

```
dcontext/                     ← workspace root
├── Cargo.toml                ← workspace manifest
├── README.md
├── docs/
│   ├── dcontext-design.md    ← this document
│   └── usage-guide.md        ← user-facing usage guide
├── samples/                  ← runnable examples (publish = false)
│   └── src/bin/
│       ├── basic_scope.rs
│       ├── cross_thread.rs
│       ├── async_tasks.rs
│       ├── async_scopes.rs
│       ├── feature_flags.rs
│       ├── cross_process.rs
│       ├── worker_pool.rs
│       ├── typed_keys.rs
│       ├── macros.rs
│       ├── scope_chain.rs
│       ├── size_limits.rs
│       ├── custom_codec.rs
│       ├── version_migration.rs
│       └── dactor_propagation.rs
├── dcontext/                 ← core crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs            ← public API (all free functions)
│       ├── store.rs          ← ContextStore, CONTEXT thread_local
│       ├── registry.rs       ← Registry, RegistryBuilder, DI
│       ├── scope.rs          ← ScopeNode, ScopeGuard
│       ├── snapshot.rs       ← ContextSnapshot
│       ├── future_ext.rs     ← WithContext<F>, ContextFutureExt
│       ├── attach.rs         ← AttachGuard
│       ├── wire.rs           ← WireContext serialization
│       ├── error.rs          ← ContextError
│       ├── config.rs         ← size / scope-chain limits
│       ├── context_key.rs    ← ContextKey<T> (feature: context-key)
│       ├── macros.rs         ← register_contexts!
│       ├── value.rs          ← ContextValue trait + blanket impl
│       └── tests.rs          ← comprehensive test suite
└── dcontext-dactor/          ← dactor actor integration crate
```

---

## 13. Usage Examples

### 13.1 Basic Sync Usage

```rust
use dcontext::{initialize, push_scope, set_context_variable, get_context_variable, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    initialize(builder);

    let _scope = push_scope("request");
    set_context_variable("request_id", RequestId("req-123".into()));
    handle_request();
}

fn handle_request() {
    let rid = get_context_variable::<RequestId>("request_id").unwrap();
    println!("Handling {}", rid.0);
}
```

### 13.2 Cross-Thread Propagation

```rust
use dcontext::{fork, attach_store, set_context_variable, get_context_variable};

set_context_variable("request_id", RequestId("req-789".into()));

let child_store = fork();
let handle = std::thread::spawn(move || {
    let _guard = attach_store(child_store);
    let rid = get_context_variable::<RequestId>("request_id").unwrap();
    println!("Worker sees: {}", rid.0);
});
handle.join().unwrap();
```

### 13.3 Async Task Propagation

```rust
use dcontext::{ContextFutureExt, set_context_variable, get_context_variable};

set_context_variable("request_id", RequestId("req-async".into()));

tokio::spawn(async {
    let rid = get_context_variable::<RequestId>("request_id").unwrap();
    println!("Async task sees: {}", rid.0);
}.fork()).await.unwrap();
```

### 13.4 Cross-Process Propagation

```rust
// Sender
let bytes = dcontext::capture().serialize().unwrap();
request.set_header("x-context-bin", base64::encode(&bytes));

// Receiver
let bytes = base64::decode(request.header("x-context-bin")).unwrap();
let snap = dcontext::ContextSnapshot::deserialize(&bytes).unwrap();
let _guard = dcontext::attach_snapshot(snap);
// Values are now accessible via get_context_variable(...)
```

### 13.5 HTTP Middleware Pattern

```rust
use dcontext::{attach_snapshot, capture, ContextFutureExt, ContextSnapshot, push_scope};

async fn handle(request: Request) -> Response {
    // Restore inbound context
    let snap = if let Some(bytes) = request.context_bytes() {
        ContextSnapshot::deserialize(bytes).unwrap()
    } else {
        ContextSnapshot::default()
    };

    // Run handler with restored context + named scope
    async {
        dcontext::set_context_variable("request_id", request.id().to_string());
        route_handler(request).await
    }
    .scope("http_request")
    .attach(snap)
    .await
}
```

---

## 14. Thread Safety Summary

| Component | Synchronization | Notes |
|-----------|----------------|-------|
| Registry (frozen) | `OnceLock` | Write once at startup, lock-free reads at runtime |
| Registry (build) | `Mutex` | Only used in tests and before `initialize()` |
| ContextStore | `thread_local! / Cell` | No cross-thread sharing; each thread has its own |
| WithContext | `Cell` swap per poll | Store is owned by the wrapper, swapped in/out |
| ContextSnapshot | `Arc<HashMap>` | Immutable after creation; `Send + Sync` |
| Value access | None (thread-local) | Lock-free on hot path |

---

## 15. Future Work

- **Middleware integrations** for popular web frameworks (axum, actix-web, tonic).
- **Metrics** — track context snapshot/restore frequency, serialization overhead.
- **Lazy values** — context entries that are computed on first access.
- **OpenTelemetry interop** — bridge that imports OTel baggage into `dcontext` on inbound requests and exports `dcontext` values as OTel baggage on outbound calls.

---

## Appendix: Removed APIs (0.8.x → 0.9.0)

The following modules and APIs were removed:

| Removed | Replacement |
|---------|-------------|
| `sync_ctx` module | Crate-root free functions |
| `async_ctx` module | `ContextFutureExt` trait |
| `dcontext-tracing` crate | Removed entirely |
| Tokio dependency | `WithContext` future wrapper (runtime-agnostic) |
| `set_context` / `get_context` | `set_context_variable` / `get_context_variable` |
| `serialize_context()` | `capture().serialize()` |
| `deserialize_context(bytes)` | `ContextSnapshot::deserialize(bytes)` + `attach_snapshot` |
| `push_scope_with_snapshot` | `push_scope()` + `merge_with()` |
| `register_local` standalone | `register_with(key, \|o\| o.local_only())` |
| `ContextValue::is_local()` | Registration-time `local_only` flag |
| `spawn_with_context` | `std::thread::spawn` + `fork()` + `attach_store()` |
| `spawn_with_context_async` | `.fork()` on futures |
