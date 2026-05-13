# dcontext — Detailed Design

> **v0.8.0 Architecture Change — Dual-Context Model (AsyncContext + SyncContext)**
>
> Starting with v0.8.0, dcontext introduces a **dual-context model** that
> separates async (task-local) and sync (thread-local) storage into independent
> modules. This eliminates the scope chain leak bug caused by depth mismatches
> between `DcontextLayer` and application code sharing the same store.
>
> Key changes:
>
> - **`dcontext::async_ctx`** module — all operations target the tokio task-local
>   store exclusively. No-ops gracefully outside async tasks.
>
> - **`dcontext::sync_ctx`** module — all operations target the thread-local
>   store exclusively. Always succeeds.
>
> - **`AsyncDcontextLayer`** — tracing layer that pushes/pops scopes on the
>   task-local store. Scopes persist across yields (push on first enter, pop
>   on close). Structurally prevents the monotonic scope chain growth bug.
>
> - **`SyncDcontextLayer`** — tracing layer that pushes/pops scopes on the
>   thread-local store via standard enter/exit lifecycle.
>
> - **One-way bridging** — `async_ctx::snapshot()` + `sync_ctx::restore()` for
>   explicit async→sync context transfer.
>
> - **No shared mutable state** — the two stores never interfere with each other.
>   The old root-level dispatching APIs were removed; callers now choose
>   `sync_ctx` or `async_ctx` explicitly.
>
> - **Compatibility surface** — registration types, snapshots, spawn helpers,
>   and tracing-layer aliases remain at the crate root where documented.

> **v0.4.0 Architecture Change — Cell-based Contention-free Storage**
>
> Starting with v0.4.0, the internal storage was redesigned to eliminate
> `RefCell` re-entrancy panics. The key changes:
>
> - **`Cell<Option<ContextStore>>`** replaces `RefCell<ContextStack>` in both
>   thread-local and task-local storage. The `Cell` take/set pattern ensures
>   that no user code (Clone, Drop, callbacks) runs while the store is
>   temporarily moved out. Re-entrant access during the brief "busy" window
>   sees `None` and returns defaults gracefully — no panics.
>
> - **`Arc<dyn ContextValue>`** replaces `Box<dyn ContextValue>` for all
>   stored values. Scope push/pop only bumps reference counts (no user Clone).
>   Values are shared cheaply across scope chain nodes and snapshots.
>
> - **Sparse scopes with parent-chain fallback**: each scope only stores keys
>   that were explicitly set in that scope. Reads walk the parent chain
>   (`O(depth)`). Keys registered with `.cached()` are eagerly copied
>   (`Arc::clone`) into each new scope for `O(1)` reads.
>
> - **`ScopeNode`** (immutable, `Arc`-wrapped, `Send + Sync`) forms a
>   persistent linked list of frozen parent scopes. `Arc::try_unwrap` on
>   `leave_scope` enables zero-copy restoration when the node is unshared.
>
> - **`update_context`** API: read-modify-write as three separate operations.
>   The callback runs with the store fully available (not "busy"), so
>   re-entrant reads work. Last-writer-wins on concurrent updates — by design.
>
> The descriptions below of `RefCell`, `ContextStack`, and borrow safety
> reflect the pre-v0.4.0 architecture and are retained for historical reference.

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

`dcontext` fills all four gaps with a single propagation toolkit built around
explicit sync and async context modules.

### Design Principles

| Principle | Description |
|-----------|-------------|
| **Type-safe** | Values are stored as `Any` but retrieved with compile-time type checking via generics. |
| **Scoped** | Context modifications in a child scope are invisible after the scope exits. |
| **Portable** | Context can be serialized for cross-process propagation; the library handles the plumbing. |
| **Tokio-based async** | Async context propagation is built around Tokio task-local storage. Non-Tokio runtime support was removed in v0.8.0. |
| **Zero-cost when unused** | Thread-local storage means no global locks on the hot path. |

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
In the example above, `get_context::<TraceId>("trace_id")` returns `TraceId("abc")` (scope 2 shadows scope 0).

**Set** always writes to the topmost (current) scope, creating a shadow entry.

**Leave scope** drops scope 2, revealing the parent state — `trace_id` reverts to `"xyz"` and `flags` to `{ debug: false }`.

### 2.2 Type-Erased Storage

Each value in the map is stored as a **`ContextValue`** trait object:

```rust
trait ContextValue: Any + Send + Sync {
    fn clone_boxed(&self) -> Box<dyn ContextValue>;
    fn as_any(&self) -> &dyn Any;
    fn serialize_value(&self) -> Result<Vec<u8>, ContextError>;
}
```

A blanket implementation covers all `T: Clone + Send + Sync + Serialize + DeserializeOwned + 'static`.

> **RefCell safety (C3):** When `get_context` retrieves a value, it must
> `clone_boxed()` the trait object **while** the `RefCell` is borrowed, then
> **drop the borrow before** downcasting and returning. This prevents
> re-entrancy panics if a value's `Clone` impl itself calls `get_context`.
> See §5.4 for the concrete algorithm.

### 2.3 Context Registration

Before a context key can be used, its type must be **registered**. Registration records:

| Field | Type | Purpose |
|-------|------|---------|
| `key` | `&'static str` | Human-readable name, used as map key, serialization, and diagnostics |
| `type_id` | `TypeId` | Rust `TypeId` of the concrete struct — used for type-safety validation |
| `default_fn` | `fn() -> Box<dyn ContextValue>` | Factory that produces the default value (from `Default` impl) |
| `deserialize_fn` | `fn(&[u8], u32) -> Result<Box<dyn ContextValue>, ContextError>` | Deserializer for restoring from bytes (receives key version) |

Registration is typically done once at startup:

```rust
let mut builder = dcontext::RegistryBuilder::new();
builder.register::<TraceContext>("trace_context");
builder.register::<FeatureFlags>("feature_flags");
dcontext::initialize(builder);
```

The registry is a global `RwLock<HashMap<&'static str, Registration>>`.
Reads (which dominate) take a read lock; registration (startup only) takes a
write lock. The registry is a **cold path** — it is consulted only during
`register()` and deserialization. The hot path (`get_context` / `set_context`)
operates on the `ContextStack` in thread-local / task-local storage, not the
registry. `TypeId` is stored in each `Registration` for runtime type-safety
checks but is not used as a map key.

#### 2.3.1 Typed Key Wrappers (Optional Ergonomic API)

For compile-time safety against key typos and type mismatches, the library
provides an optional `ContextKey<T>` newtype:

```rust
/// A typed handle to a registered context entry.
pub struct ContextKey<T: 'static> {
    key: &'static str,
    _marker: PhantomData<T>,
}

impl<T> ContextKey<T>
where
    T: Clone + Default + Send + Sync + Serialize + DeserializeOwned + 'static,
{
    /// Register and return a typed key handle.
    pub fn new(key: &'static str) -> Self { ... }
}
```

Usage:

```rust
static REQUEST_ID: ContextKey<RequestId> = ContextKey::new("request_id");

// No turbofish, no string key at call site:
let rid = REQUEST_ID.get();     // returns RequestId
REQUEST_ID.set(new_value);
```

The string-based API (`get_context::<T>(key)`) remains available for dynamic
use cases.

---

## 3. Scope Tree

### 3.1 Scope Representation

```rust
struct Scope {
    /// Overlay values set in this scope (shadows parent entries).
    values: HashMap<&'static str, Box<dyn ContextValue>>,
}

struct ContextStack {
    /// Stack of scopes, last element is the current (innermost) scope.
    scopes: Vec<Scope>,
    /// Monotonically increasing scope counter for guard validation.
    next_scope_id: u64,
    /// Index cache: maps each effective key to the scope index holding its value.
    /// Eagerly maintained — always valid. Stores only indices, never user data.
    /// No user Clone code runs during cache operations (C3 re-entrancy safe).
    read_cache: HashMap<&'static str, usize>,
}
```

The `ContextStack` lives in **thread-local** (sync) or **task-local** (async) storage.

> **Lookup optimization (S3):** Reads are expected to dominate writes. The
> `read_cache` maps each effective key to the scope index holding its value,
> providing O(1) lookups via `HashMap::get` + direct scope access. The cache
> stores only `(&'static str, usize)` pairs — no user values are cloned,
> preserving C3 re-entrancy safety. Cache maintenance: `push_scope` requires
> no update (empty scope), `pop_scope` rebuilds (O(total keys)), `set` does
> O(1) incremental update.

### 3.2 Scope Lifecycle

| Operation | Effect |
|-----------|--------|
| `enter_scope()` | Pushes a new empty `Scope` onto the stack. No cache update needed (empty scope). Returns a `ScopeGuard`. |
| `leave_scope()` / drop `ScopeGuard` | Validates scope ID, pops the scope, rebuilds `read_cache`. |
| `get_context::<T>(key)` | O(1) lookup via `read_cache` index → scope access, downcasts to `T`. Returns `T` (cloned) or default. |
| `set_context(key, value)` | Inserts/replaces in the current (topmost) scope. O(1) incremental `read_cache` update. |

### 3.3 ScopeGuard

The `ScopeGuard` ensures scopes are properly cleaned up via RAII and
**validates drop order** to detect misuse:

```rust
pub struct ScopeGuard {
    /// The scope ID assigned when this guard was created.
    scope_id: u64,
    /// The expected stack depth when this scope was pushed.
    expected_depth: usize,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        leave_scope_checked(self.scope_id, self.expected_depth);
    }
}

fn leave_scope_checked(scope_id: u64, expected_depth: usize) {
    with_current_stack(|stack| {
        let stack = stack.borrow_mut();
        assert_eq!(
            stack.scopes.len(), expected_depth,
            "ScopeGuard dropped out of order: expected depth {}, got {}. \
             Scopes must be exited in LIFO order.",
            expected_depth, stack.scopes.len()
        );
        stack.scopes.pop();
        stack.rebuild_cache(); // rebuild index cache
    });
}
```

> **Drop-order safety (C2):** Out-of-order drops panic with a clear message,
> preventing silent stack corruption.

### 3.4 Explicit Scope Pattern

The old root-level `dcontext::scope(|| ...)` helper is gone. The recommended
pattern is now an explicit guard from `sync_ctx` or `async_ctx`:

```rust
{
    let _guard = dcontext::sync_ctx::enter_scope();
    dcontext::sync_ctx::set_context("trace_id", TraceId::new());
    do_work();
}

// Guard-based APIs remain available when a named scope or a longer-lived scope
// is needed:
let _guard = dcontext::sync_ctx::enter_scope();
```

## 4. Public API

The dual-context redesign splits runtime operations into two explicit modules:

- `dcontext::sync_ctx` for thread-local state
- `dcontext::async_ctx` for Tokio task-local state

Registration, snapshots, configuration, and inheritance helpers remain at the
crate root.

### 4.1 Registration and Initialization

```rust
use dcontext::{initialize, RegistryBuilder};

let mut builder = RegistryBuilder::new();
builder.register::<RequestId>("request_id");
builder.register::<UserId>("user_id");
initialize(builder);
```

### 4.2 Sync Context API

```rust
// Scope management
pub fn dcontext::sync_ctx::enter_scope() -> ScopeGuard;
pub fn dcontext::sync_ctx::enter_named_scope(name: impl Into<String>) -> ScopeGuard;
pub fn dcontext::sync_ctx::scope_chain() -> Vec<String>;

// Value access
pub fn dcontext::sync_ctx::set_context<T>(key: &'static str, value: T);
pub fn dcontext::sync_ctx::get_context<T>(key: &str) -> Option<T>;
pub fn dcontext::sync_ctx::update_context<T>(key: &'static str, f: impl FnOnce(T) -> T);

// Propagation
pub fn dcontext::sync_ctx::snapshot() -> ContextSnapshot;
pub fn dcontext::sync_ctx::attach(snapshot: ContextSnapshot) -> ScopeGuard;
pub fn dcontext::sync_ctx::restore(snapshot: ContextSnapshot);

// Cross-process
pub fn dcontext::sync_ctx::serialize_context() -> Result<Vec<u8>, ContextError>;
pub fn dcontext::sync_ctx::deserialize_context(bytes: &[u8]) -> Result<ScopeGuard, ContextError>;
```

### 4.3 Async Context API

```rust
pub fn dcontext::async_ctx::set_context<T>(key: &'static str, value: T);
pub fn dcontext::async_ctx::get_context<T>(key: &str) -> Option<T>;
pub fn dcontext::async_ctx::update_context<T>(key: &'static str, f: impl FnOnce(T) -> T);
pub fn dcontext::async_ctx::scope_chain() -> Vec<String>;
pub fn dcontext::async_ctx::snapshot() -> ContextSnapshot;
pub fn dcontext::async_ctx::attach(snapshot: ContextSnapshot) -> ScopeGuard;
pub fn dcontext::async_ctx::serialize_context() -> Result<Vec<u8>, ContextError>;
pub fn dcontext::async_ctx::deserialize_context(bytes: &[u8]) -> Result<ScopeGuard, ContextError>;
pub async fn dcontext::async_ctx::with_context<F>(snapshot: ContextSnapshot, fut: F) -> F::Output;
pub async fn dcontext::async_ctx::scope<F>(name: &str, fut: F) -> F::Output;
```

### 4.4 Root-Level Helpers That Remain

```rust
pub struct ContextSnapshot;
pub struct RegistryBuilder;
pub fn initialize(builder: RegistryBuilder);

// Spawn helpers (capture + spawn)
pub fn spawn_with_sync_context(mode: ContextInheritance, future: F) -> JoinHandle<...>;
pub fn spawn_with_async_context(mode: ContextInheritance, future: F) -> JoinHandle<...>;
pub fn spawn_blocking_with_async_context(mode: ContextInheritance, f: F) -> JoinHandle<...>;

// Wrap helpers (capture only, caller spawns)
pub fn wrap_with_sync_context(mode: ContextInheritance, future: F) -> impl Future;
pub fn wrap_with_async_context(mode: ContextInheritance, future: F) -> impl Future;

// Wire deserialization without store mutation
pub fn deserialize_to_snapshot(bytes: &[u8]) -> Result<ContextSnapshot, ContextError>;

pub fn register_contexts!(...);
```

### 4.5 Context Inheritance: Fork vs Snapshot

All spawn and wrap helpers accept a `ContextInheritance` mode:

```rust
pub enum ContextInheritance {
    Fork,      // O(current_scope_keys) — Arc-shared frozen parent
    Snapshot,  // O(depth × keys) — full deep copy
}
```

**Fork** creates a child `ContextStore` whose `frozen_parent` points to the
current scope via `Arc`. Value reads fall through to the frozen parent; writes
are isolated (copy-on-write). This is the cheapest option and the default.

**Snapshot** walks the entire scope chain, clones all effective values into a
flat `HashMap`, and wraps it in an `Arc`. The result is a self-contained
`ContextSnapshot` with no references back to the parent store.

#### When to use each

| Scenario | Mode | Why |
|----------|------|-----|
| `tokio::spawn` child task | **Fork** | Parent outlives child; cheap Arc sharing |
| `spawn_blocking` thread | **Fork** | Same as above; blocking thread is short-lived |
| Task queue / bounded channel | **Fork** via `wrap_with_*` | Capture is cheap; future carries its own scope |
| Cross-thread message passing | **Snapshot** | Self-contained; sender's context may be gone when receiver reads |
| Retry / rate-limit wrappers | **Fork** via `wrap_with_*` | Each attempt gets its own isolated scope |

#### Anti-pattern: Fork for message passing

Do **not** send a forked store across a message channel to a different thread.
Fork creates a live `Arc` reference to the sender's scope chain. If the sender
pops scopes or exits before the receiver reads, the forked child sees stale
state. Use `Snapshot` (via `sync_ctx::snapshot()`) for messages instead.

### 4.6 Wrap Helpers: Capture Now, Spawn Later

The spawn helpers (`spawn_with_*`) couple context capture with task spawning.
When you need to capture context but spawn later — e.g., for backpressure,
ordering, concurrency control — use the wrap helpers:

```rust
// capture sync context into a wrapped future
let wrapped = dcontext::wrap_with_sync_context(
    dcontext::ContextInheritance::Fork,
    async { handle_request().await },
);

// enqueue on a bounded channel (backpressure)
sender.try_send(Box::pin(wrapped))?;

// consumer spawns when ready
let task = receiver.recv().await.unwrap();
tokio::spawn(task);
```

The wrapped future carries its own task-local context scope. It can be stored,
queued, retried, or awaited directly without spawning.

### 4.7 Direct Wire Deserialization

`deserialize_to_snapshot` converts wire bytes directly into a `ContextSnapshot`
without touching any live context store:

```rust
// Old pattern: mutate store, snapshot, drop
let _guard = dcontext::sync_ctx::deserialize_context(&bytes)?;
let snap = dcontext::sync_ctx::snapshot();
drop(_guard);

// New pattern: direct, no side effects
let snap = dcontext::deserialize_to_snapshot(&bytes)?;
```

This is especially useful on sync actor threads (no task-local store) or when
you need to inspect wire context without side effects.

```rust
let _guard = dcontext::sync_ctx::enter_scope();
dcontext::sync_ctx::set_context("request_id", RequestId("req-999".into()));

let ctx = dcontext::sync_ctx::snapshot();

third_party::do_work_async(move || {
    let _guard = dcontext::sync_ctx::attach(ctx);
    let rid = dcontext::sync_ctx::get_context::<RequestId>("request_id").unwrap();
    println!("Callback sees: {}", rid.0);
});
```

## 5. Storage Strategy

### 5.1 Thread-Local Storage (`sync_ctx`)

The sync store lives in a `thread_local!` cell. All `sync_ctx::*` operations are
resolved directly against that thread-local state.

### 5.2 Task-Local Storage (`async_ctx`)

The async store lives in Tokio `task_local!` storage. `async_ctx::with_context`
establishes the task-local store for a future, and `async_ctx::scope` pushes a
scope for the duration of an async operation.

```rust
tokio::task_local! {
    static TASK_CONTEXT: Cell<Option<ContextStore>>;
}
```

### 5.3 Explicit Separation Instead of Dispatch

Pre-v0.8 designs routed `get_context`/`set_context` through runtime detection.
That dispatch layer is gone. Callers now choose the storage backend directly:

- `sync_ctx::*` for synchronous code and blocking threads
- `async_ctx::*` for Tokio tasks

This removes ambiguity, avoids sync/async depth mismatches, and makes scope
ownership explicit at every boundary.

### 5.4 RefCell Borrow Safety

To prevent re-entrancy panics when a value's `Clone` impl calls back into the
context API, reads clone the type-erased value while the store is borrowed,
release the borrow, and only then downcast/clone the concrete `T`.

```rust
fn get_context_internal<T: Clone + 'static>(...) -> Option<T> {
    // 1. Borrow store and clone Arc/boxed value.
    // 2. Release borrow.
    // 3. Downcast and clone T outside the borrow window.
}
```

## 6. Serialization Wire Format

### 6.1 Format

The serialized context is a **bincode-encoded** map:

```rust
#[derive(Serialize, Deserialize)]
struct WireContext {
    /// Format version for forward compatibility.
    version: u32,
    /// Key → serialized value bytes.
    entries: Vec<WireEntry>,
}

#[derive(Serialize, Deserialize)]
struct WireEntry {
    key: String,
    /// Per-key schema version, supplied by the registration.
    /// Allows the deserializer to handle schema evolution per type.
    key_version: u32,
    /// Serialized value bytes (inner serialization).
    value: Vec<u8>,
}
```

The outer container uses bincode; each value is independently serialized (also
bincode by default). This two-level scheme allows the receiver to skip unknown
keys gracefully.

> **Per-key versioning (I3):** Each `WireEntry` carries a `key_version` so
> that the deserializer can detect schema changes per type. The
> `deserialize_fn` in the registry receives the `key_version` and can apply
> migration logic or reject incompatible versions.

> **Pluggable codec (S4):** Bincode is fast but not self-describing and not
> stable across versions/architectures by default. For cross-language or
> long-lived wire formats, users can register a custom per-key codec via
> `register_with_codec`. The custom `encode`/`decode` functions replace
> bincode for the inner value serialization. The top-level `WireContext`
> envelope remains bincode. See the `custom_codec` sample.

### 6.2 Version Compatibility

- `version = 1` is the initial format.
- Future versions may add fields to `WireContext` (bincode is not self-describing, so version checks are required).
- Unknown keys in `entries` are silently ignored on deserialization (the receiver only restores keys it has registered).
- Per-key `key_version` mismatches are reported as `ContextError::DeserializationFailed` with a descriptive message.

---

## 7. Error Handling

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

> **Size limits (S6):** A configurable maximum context size (in bytes) can be
> set via `dcontext::set_max_context_size(limit)`. Serialization and snapshot
> operations check against this limit and return `ContextTooLarge` if
> exceeded. Default: no limit (backward compatible).


---

## 8. Macro Support

### 8.1 Registration Macro

```rust
register_contexts!(builder, {
    "trace_id" => TraceId,
    "flags" => Flags,
});
```

Expands to individual registration calls on the supplied `RegistryBuilder`.

### 8.2 Scoped Context Pattern

`with_scope!` was removed. The replacement is an explicit guard:

```rust
let _guard = dcontext::sync_ctx::enter_scope();
dcontext::sync_ctx::set_context("trace_id", TraceId::new());
dcontext::sync_ctx::set_context("flags", Flags { debug: true });
do_work();
```

---

## 9. Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `tokio` | **yes** | Enables `async_ctx` and Tokio task-local propagation. |
| `base64` | **yes** | Enables base64-oriented serialization helpers for text transports. |
| `context-key` | **yes** | Enables `ContextKey<T>` typed key wrapper for compile-time safe access. |

> **[Removed in v0.8.0] `context-future`:** ~~A non-default feature for
> runtime-agnostic async propagation.~~ The library is now Tokio-only.

---

## 10. Crate Structure

```
dcontext/                     ← workspace root
├── Cargo.toml                ← workspace manifest
├── README.md
├── docs/
│   ├── dcontext-design.md    ← this document
│   └── review_comment.md     ← design review comments
├── samples/                  ← runnable examples (publish = false)
│   └── src/bin/
│       ├── basic_scope.rs
│       ├── cross_thread.rs
│       ├── async_tasks.rs
│       ├── feature_flags.rs
│       ├── cross_process.rs
│       ├── worker_pool.rs
│       ├── typed_keys.rs
│       ├── macros.rs         ← register_contexts!
│       ├── async_scopes.rs   ← async_ctx::scope
│       ├── dual_sync_ctx.rs
│       ├── dual_async_ctx.rs
│       ├── dual_bridging.rs
│       └── ...
├── dcontext/                 ← core crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs            ← public API re-exports
│       ├── sync_ctx.rs       ← explicit thread-local context API
│       ├── async_ctx.rs      ← explicit task-local context API
│       ├── registry.rs       ← type registration + extensible metadata
│       ├── scope.rs          ← ScopeGuard and scope cleanup
│       ├── snapshot.rs       ← ContextSnapshot capture/attach
│       ├── store.rs          ← ContextStore implementation
│       ├── wire.rs           ← WireContext serialization
│       ├── error.rs          ← ContextError
│       ├── config.rs         ← size / scope-chain limits
│       ├── context_key.rs    ← ContextKey<T> (feature: context-key)
│       ├── macros.rs         ← register_contexts!
│       ├── inheritance.rs    ← spawn / inheritance helpers
│       └── value.rs          ← ContextValue trait + blanket impl
├── dcontext-tracing/         ← tracing integration crate
└── dcontext-dactor/          ← dactor actor integration crate
```

## 11. Usage Examples

### 11.1 Basic Sync Usage

```rust
use dcontext::{initialize, sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    initialize(builder);

    {
        let _guard = sync_ctx::enter_scope();
        sync_ctx::set_context("request_id", RequestId("req-123".into()));
        handle_request();
    }
}

fn handle_request() {
    let rid = sync_ctx::get_context::<RequestId>("request_id").unwrap();
    println!("Handling {}", rid.0);
}
```

### 11.2 Cross-Thread Propagation

For spawning a child task/thread, use the spawn helpers with `ContextInheritance`:

```rust
use dcontext::{ContextInheritance, spawn_with_sync_context, sync_ctx};

let _guard = sync_ctx::enter_scope();
sync_ctx::set_context("request_id", RequestId("req-789".into()));

// Spawn an async task that inherits sync context (Fork = cheap)
spawn_with_sync_context(ContextInheritance::Fork, async {
    let rid = dcontext::async_ctx::get_context::<RequestId>("request_id").unwrap();
    println!("Task sees: {}", rid.0);
});
```

For cross-thread **message passing** (sender and receiver are independent),
use `Snapshot` instead of `Fork`:

```rust
use dcontext::sync_ctx;

// Sender
sync_ctx::set_context("request_id", RequestId("req-789".into()));
let snap = sync_ctx::snapshot();  // self-contained deep copy
tx.send(WorkItem { context: snap, data }).unwrap();

// Receiver (different thread)
let item = rx.recv().unwrap();
let _guard = sync_ctx::attach(item.context);
let rid = sync_ctx::get_context::<RequestId>("request_id").unwrap();
```

### 11.3 Async Task Propagation

```rust
use dcontext::{async_ctx, ContextInheritance, spawn_with_async_context, sync_ctx};

let snap = {
    let _guard = sync_ctx::enter_scope();
    sync_ctx::set_context("request_id", RequestId("req-async".into()));
    sync_ctx::snapshot()
};

async_ctx::with_context(snap, async {
    // Spawn child task with Fork (cheapest)
    let handle = spawn_with_async_context(ContextInheritance::Fork, async {
        let rid = async_ctx::get_context::<RequestId>("request_id").unwrap();
        println!("Async task sees: {}", rid.0);
    });

    handle.await.unwrap();
})
.await;
```

For capture-now-spawn-later patterns (e.g., task queues with backpressure):

```rust
use dcontext::{ContextInheritance, wrap_with_sync_context};

// On sync actor thread: capture context, wrap future, enqueue
let wrapped = wrap_with_sync_context(
    ContextInheritance::Fork,
    async { handle_rpc().await },
);
bounded_channel.try_send(Box::pin(wrapped))?;

// Consumer loop: spawn when ready
let task = bounded_channel.recv().await.unwrap();
tokio::spawn(task);
```

### 11.4 Cross-Process Propagation

```rust
let bytes = dcontext::sync_ctx::serialize_context().unwrap();
request.headers_mut().insert("x-context-bin", bytes.clone().into());

let inbound = request.headers().get("x-context-bin").unwrap();
let _guard = dcontext::sync_ctx::deserialize_context(inbound.as_bytes()).unwrap();
// Registered values are now available via dcontext::sync_ctx::get_context(...)
```

### 11.5 Integrating with `tracing`

For synchronous code, install `SyncDcontextLayer` and read values from
`dcontext::sync_ctx`. For asynchronous services, install `AsyncDcontextLayer`
and read values from `dcontext::async_ctx` inside the request task.

```rust
tracing_subscriber::registry()
    .with(dcontext_tracing::AsyncDcontextLayer::new())
    .init();
```

### 11.6 HTTP Middleware Pattern

A typical async server flow is:

1. Deserialize incoming wire bytes directly into a snapshot (no store mutation).
2. Hand that snapshot to `async_ctx::with_context` for the request future.
3. Use `async_ctx::scope` or `async_ctx::set_context` inside handlers.
4. Serialize with `async_ctx::serialize_context` when forwarding.

```rust
let snap = match inbound_header_bytes {
    Some(bytes) => dcontext::deserialize_to_snapshot(bytes)
        .unwrap_or_default(),
    None => dcontext::ContextSnapshot::default(),
};

dcontext::async_ctx::with_context(snap, async move {
    dcontext::async_ctx::scope("http_request", async {
        dcontext::async_ctx::set_context("request_id", request_id.clone());
        handle_request().await;
    })
    .await;
})
.await;
```

## 12. Thread Safety Summary

| Component | Synchronization | Notes |
|-----------|----------------|-------|
| Registry | `RwLock` (global) | Write at startup, read-only at runtime |
| ContextStack (sync) | `thread_local! / RefCell` | No cross-thread sharing; each thread has its own |
| ContextStack (async) | `task_local! / RefCell` | Follows the task across thread migrations |
| ContextSnapshot | `Arc<HashMap>` | Immutable after creation; `Clone + Send + Sync` |
| `get_context` / `set_context` | None (thread-local or task-local) | Lock-free on hot path |
| Dual-storage dispatch | `TASK_CONTEXT.try_with()` | No lock; falls back to thread-local only in pure sync |

---

## 13. Future Work

- ~~**Wire version migration support**~~ ✅ Implemented as `register_migration` — register a conversion function from an old wire version to the current type. The `deserializers` map in `Registration` holds multiple versioned deserializers per key. See the `version_migration` sample.
- ~~**Local-only (non-serializable) context entries**~~ ✅ Implemented as `register_local` / `set_context_local`. Local-only entries propagate via `snapshot()`/`attach()` but are silently excluded from serialization. Types only need `Clone+Default+Send+Sync` (no `Serialize`).
- ~~**Sample usage programs**~~ ✅ 14 samples covering all implemented features.
- ~~**`async-std` support** via poll-wrapper `ContextFuture` (see §5.2 / §9).~~ **[Removed in v0.8.0]** `ContextFuture` / `with_context_future` and runtime-agnostic executor support were removed when the library standardized on Tokio.
- **Automatic propagation** via runtime hooks (e.g., Tokio's `tracing` integration).
- **Middleware integrations** for popular web frameworks (axum, actix-web, tonic).
- ~~**Context size limits** enforcement (configurable max, `ContextTooLarge` error).~~ ✅ Implemented as `set_max_context_size`.
- **Metrics** — track context snapshot/restore frequency, serialization overhead.
- **Lazy values** — context entries that are computed on first access.
- ~~**Pluggable top-level codec**~~ ✅ Implemented as `register_with_codec` — per-key custom serialize/deserialize functions. The top-level wire format envelope remains bincode; inner values can use any format (JSON, MessagePack, etc.).
- **`tracing` / OpenTelemetry interop (S5):** ~~Define how `dcontext` relates
  to `tracing::Span` and `opentelemetry::Context`.~~ Partially implemented:
  - ✅ `dcontext-tracing` crate with `DcontextLayer` — span↔context data bridge (field extraction, log enrichment, span recording)
  - ✅ Field mapping — span fields automatically populate context values
  - ✅ Log enrichment — `WithContextFields` formatter injects context into log output
  - ✅ Extensible per-key metadata system for cross-crate annotations
  - A bridge that imports OTel baggage into `dcontext` on inbound requests
    and exports `dcontext` values as OTel baggage on outbound calls.
  - `dcontext` is not a replacement for `tracing` — it handles arbitrary
    application context, while `tracing` focuses on structured diagnostics.
    The two are complementary.
