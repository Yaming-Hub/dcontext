# dcontext — Detailed Design

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

`dcontext` fills all four gaps with a single unified API.

### Design Principles

| Principle | Description |
|-----------|-------------|
| **Type-safe** | Values are stored as `Any` but retrieved with compile-time type checking via generics. |
| **Scoped** | Context modifications in a child scope are invisible after the scope exits. |
| **Portable** | Context can be serialized for cross-process propagation; the library handles the plumbing. |
| **Runtime-agnostic** | Core logic has no async runtime dependency. Async integration (Tokio, async-std) is opt-in via cargo features. |
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

### 2.3 Context Registration

Before a context key can be used, its type must be **registered**. Registration records:

| Field | Type | Purpose |
|-------|------|---------|
| `key` | `&'static str` | Unique name for this context entry |
| `type_id` | `TypeId` | Rust `TypeId` of the concrete struct |
| `default_fn` | `fn() -> Box<dyn ContextValue>` | Factory that produces the default value (from `Default` impl) |
| `deserialize_fn` | `fn(&[u8]) -> Result<Box<dyn ContextValue>, ContextError>` | Deserializer for restoring from bytes |

Registration is typically done once at startup:

```rust
dcontext::register::<TraceContext>("trace_context");
dcontext::register::<FeatureFlags>("feature_flags");
```

The registry is a global `RwLock<HashMap<&'static str, Registration>>`. Reads (which dominate) take a read lock; registration (startup only) takes a write lock.

---

## 3. Scope Tree

### 3.1 Scope Representation

```rust
struct Scope {
    /// Overlay values set in this scope (shadows parent entries).
    values: HashMap<String, Box<dyn ContextValue>>,
}

struct ContextStack {
    /// Stack of scopes, last element is the current (innermost) scope.
    scopes: Vec<Scope>,
}
```

The `ContextStack` lives in **thread-local** (sync) or **task-local** (async) storage.

### 3.2 Scope Lifecycle

| Operation | Effect |
|-----------|--------|
| `enter_scope()` | Pushes a new empty `Scope` onto the stack. Returns a `ScopeGuard`. |
| `leave_scope()` / drop `ScopeGuard` | Pops the top scope, reverting all changes made in it. |
| `get_context::<T>(key)` | Searches scopes top-down for `key`, downcasts to `T`. Returns `T` (cloned) or default. |
| `set_context(key, value)` | Inserts/replaces in the current (topmost) scope. |

### 3.3 ScopeGuard

The `ScopeGuard` ensures scopes are properly cleaned up via RAII:

```rust
pub struct ScopeGuard {
    _private: (), // prevent manual construction
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        leave_scope_internal();
    }
}
```

Usage:

```rust
{
    let _guard = dcontext::enter_scope();
    dcontext::set_context("trace_id", TraceId::new());
    do_work(); // sees the new trace_id
} // _guard drops → scope reverts
```

---

## 4. Public API

### 4.1 Core Functions

```rust
/// Register a context type. Must be called before get/set for this key.
/// Panics if the key is already registered with a different type.
pub fn register<T>(key: &'static str)
where
    T: Clone + Default + Send + Sync + Serialize + DeserializeOwned + 'static;

/// Enter a new scope. Returns a guard that reverts the scope on drop.
pub fn enter_scope() -> ScopeGuard;

/// Get a context value. Returns a clone of the value if found,
/// or `T::default()` if the key is registered but not set.
/// Panics if the key is not registered.
pub fn get_context<T>(key: &'static str) -> T
where
    T: Clone + Default + Send + Sync + 'static;

/// Set a context value in the current scope.
/// Panics if the key is not registered or if T doesn't match the registered type.
pub fn set_context<T>(key: &'static str, value: T)
where
    T: Clone + Send + Sync + 'static;

/// Try-get variant that returns Option<T> instead of defaulting.
pub fn try_get_context<T>(key: &'static str) -> Option<T>
where
    T: Clone + Send + Sync + 'static;
```

### 4.2 Serialization / Deserialization

For cross-process propagation, the library serializes **all** context values in the current effective context (merged view of all scopes):

```rust
/// Serialize the current context (all scopes merged) into bytes.
/// Only includes keys that have been explicitly set (not defaults).
pub fn serialize_context() -> Result<Vec<u8>, ContextError>;

/// Serialize the current context into a base64-encoded string
/// (convenient for HTTP headers, gRPC metadata).
pub fn serialize_context_string() -> Result<String, ContextError>;

/// Restore context from bytes. Pushes a new scope containing
/// the deserialized values.
pub fn deserialize_context(bytes: &[u8]) -> Result<ScopeGuard, ContextError>;

/// Restore context from a base64-encoded string.
pub fn deserialize_context_string(encoded: &str) -> Result<ScopeGuard, ContextError>;
```

### 4.3 Snapshot / Clone

For crossing sync ↔ async boundaries, spawning threads, or passing context
into third-party callbacks:

```rust
/// Capture a snapshot of the current effective context.
/// The snapshot is a self-contained, cheaply cloneable value.
pub fn snapshot() -> ContextSnapshot;

/// Attach a snapshot, entering a new scope with its values.
/// Returns a ScopeGuard.
pub fn attach(snapshot: ContextSnapshot) -> ScopeGuard;
```

`ContextSnapshot` is `Clone + Send + Sync`, making it safe to move across threads and tasks.

#### 4.3.1 Third-Party Callback Pattern

When the application passes a callback to a third-party library and that
library spawns a thread (or otherwise invokes the callback on an unknown
context), the application has **no control over thread creation** and cannot
use `spawn_with_context`. In this case, the application must explicitly
capture and restore context:

```rust
// Application registers context as usual.
register::<RequestId>("request_id");

let _guard = enter_scope();
set_context("request_id", RequestId("req-999".into()));

// Capture before handing off to third-party code.
let ctx = dcontext::snapshot();

third_party::do_work_async(move || {
    // We are now on an unknown thread spawned by the library.
    // Explicitly restore context.
    let _guard = dcontext::attach(ctx);

    // Context is available for the duration of this callback.
    let rid: RequestId = get_context("request_id");
    println!("Callback sees: {}", rid.0); // "req-999"
});
```

To reduce boilerplate, the library provides a **callback wrapper** that
captures the current context and restores it automatically when invoked:

```rust
/// Wrap a closure so that the current context is automatically
/// captured now and restored when the closure is later called
/// (potentially on a different thread).
pub fn wrap_with_context<F, T>(f: F) -> impl FnOnce() -> T + Send
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static;

/// Same as above but returns an `Fn` (for callbacks that may be
/// invoked multiple times).
pub fn wrap_with_context_fn<F, T>(f: F) -> impl Fn() -> T + Send + Sync
where
    F: Fn() -> T + Send + Sync + 'static,
    T: Send + 'static;
```

Usage with a third-party library:

```rust
let ctx_callback = dcontext::wrap_with_context(move || {
    // Context is automatically restored here.
    let rid: RequestId = get_context("request_id");
    println!("Callback sees: {}", rid.0);
});

// Hand the wrapped callback to the library — context travels with it.
third_party::do_work_async(ctx_callback);
```

### 4.4 Thread Helpers

```rust
/// Spawn a std::thread that inherits the current context.
pub fn spawn_with_context<F, T>(name: &str, f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static;
```

### 4.5 Async Helpers (feature-gated)

With the `tokio` feature:

```rust
/// Spawn a Tokio task that inherits the current context.
pub fn spawn_with_context_async<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static;

/// Run an async block with the given snapshot attached.
/// Useful for bridging sync → async.
pub async fn with_context<F, T>(snapshot: ContextSnapshot, f: F) -> T
where
    F: Future<Output = T>;
```

---

## 5. Storage Strategy

### 5.1 Thread-Local Storage

The `ContextStack` is stored in a `thread_local!`:

```rust
thread_local! {
    static CONTEXT: RefCell<ContextStack> = RefCell::new(ContextStack::new());
}
```

All `get_context` / `set_context` / `enter_scope` calls operate on this thread-local stack. This means:

- **No global locks** on read/write (fast path).
- Each thread has its own independent context stack.
- Async tasks on the same thread share the thread-local (see §5.2).

### 5.2 Async Runtime Integration

Thread-local storage is **not sufficient** for async code. In a multi-threaded
runtime (e.g., Tokio's work-stealing scheduler), a task may be suspended at any
`.await` point and resumed on a **different** OS thread. A thread-local value
set before an `.await` may belong to a completely unrelated task after resumption.

Therefore, async context propagation **must** use the runtime's built-in
task-local mechanism, which moves with the task across thread migrations:

| Runtime | Mechanism | Guarantee |
|---------|-----------|-----------|
| **Tokio** | `tokio::task_local!` | Value follows the task across `.await` points and thread migrations |
| **async-std** | `task_local!` (from `async_std::task`) | Same semantics as Tokio |
| **smol / glommio** | Thread-pinned or custom task-local | Runtime-specific; adapter required |

#### Primary storage for async: `tokio::task_local!`

When the `tokio` feature is enabled, the library declares a task-local as the
**primary** storage backend for async code:

```rust
tokio::task_local! {
    static TASK_CONTEXT: RefCell<ContextStack>;
}
```

All `get_context` / `set_context` / `enter_scope` calls inside an async task
operate on this task-local stack. Because Tokio guarantees the task-local
travels with the task, context is preserved across `.await` points regardless
of which OS thread the task resumes on.

#### Establishing the task-local scope

A task-local must be explicitly established before it can be read. The library
provides `with_context` to set up the task-local for a given future:

```rust
pub async fn with_context<F, T>(snapshot: ContextSnapshot, f: F) -> T
where
    F: Future<Output = T>,
{
    let stack = ContextStack::from_snapshot(snapshot);
    TASK_CONTEXT.scope(RefCell::new(stack), f).await
}
```

`spawn_with_context_async` combines snapshot capture + task-local setup:

```rust
pub fn spawn_with_context_async<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let snapshot = snapshot(); // capture current context
    tokio::spawn(with_context(snapshot, future))
}
```

#### Fallback: thread-local for sync code within async

When async code calls into synchronous functions that use `get_context`, the
task-local is not directly accessible (sync code cannot `.await`). The library
handles this by **mirroring** the task-local into the thread-local on scope
entry and clearing it on scope exit. Since the sync call blocks the current
thread (no `.await` → no task migration), the thread-local is safe for the
duration of the sync call.

### 5.3 Dual-Storage Dispatch

At runtime, `get_context` / `set_context` check which storage backend is
active and dispatch accordingly:

1. If a task-local `TASK_CONTEXT` is set (i.e., we are inside a
   `with_context` scope), use the task-local.
2. Otherwise, fall back to the thread-local `CONTEXT`.

This means:
- **Pure sync code** — uses thread-local (no runtime dependency).
- **Async code inside `with_context`** — uses task-local (migration-safe).
- **Sync code called from async** — uses the mirrored thread-local (safe
  because no `.await` can occur).

```rust
fn with_current_stack<R>(f: impl FnOnce(&RefCell<ContextStack>) -> R) -> R {
    // Try task-local first (async path).
    let result = TASK_CONTEXT.try_with(|stack| f(stack));
    match result {
        Ok(r) => r,
        // No task-local set — fall back to thread-local (sync path).
        Err(_) => CONTEXT.with(|stack| f(stack)),
    }
}
```

---

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
    /// Bincode-serialized value bytes (inner serialization).
    value: Vec<u8>,
}
```

The outer container uses bincode; each value is independently serialized (also bincode by default). This two-level scheme allows the receiver to skip unknown keys gracefully.

### 6.2 Version Compatibility

- `version = 1` is the initial format.
- Future versions may add fields to `WireContext` (bincode is not self-describing, so version checks are required).
- Unknown keys in `entries` are silently ignored on deserialization (the receiver only restores keys it has registered).

---

## 7. Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("context key '{0}' is not registered")]
    NotRegistered(String),

    #[error("type mismatch for key '{0}': expected {1}, got {2}")]
    TypeMismatch(String, String, String),

    #[error("serialization failed: {0}")]
    SerializationFailed(String),

    #[error("deserialization failed: {0}")]
    DeserializationFailed(String),

    #[error("no active scope")]
    NoActiveScope,
}
```

---

## 8. Macro Support

### 8.1 Registration Macro

```rust
/// Register multiple context types at once.
dcontext::register_contexts! {
    "trace_context" => TraceContext,
    "feature_flags" => FeatureFlags,
    "auth_info"     => AuthInfo,
}
```

Expands to individual `dcontext::register::<T>(key)` calls.

### 8.2 Scoped Context Macro

```rust
/// Enter a scope, set values, execute a block, and auto-revert.
dcontext::with_scope! {
    "trace_id" => TraceId::new(),
    "flags" => Flags { debug: true },
    => {
        do_work();
    }
}
```

---

## 9. Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `tokio` | **yes** | Enables Tokio task-local storage and async spawn helpers. |
| `async-std` | no | Enables async-std integration (future work). |
| `base64` | **yes** | Enables `serialize_context_string` / `deserialize_context_string`. |

---

## 10. Crate Structure

```
dcontext/                     ← workspace root
├── Cargo.toml                ← workspace manifest
├── README.md
├── docs/
│   └── dcontext-design.md    ← this document
└── dcontext/                 ← core crate
    ├── Cargo.toml
    └── src/
        ├── lib.rs            ← public API re-exports
        ├── registry.rs       ← type registration logic
        ├── scope.rs          ← Scope, ContextStack, ScopeGuard
        ├── storage.rs        ← thread-local + task-local backends
        ├── snapshot.rs       ← ContextSnapshot capture/attach
        ├── serde.rs          ← WireContext serialization
        ├── error.rs          ← ContextError
        ├── macros.rs         ← register_contexts!, with_scope!
        └── helpers.rs        ← spawn_with_context, async helpers
```

---

## 11. Usage Examples

### 11.1 Basic Sync Usage

```rust
use dcontext::{register, enter_scope, get_context, set_context};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

fn main() {
    register::<RequestId>("request_id");

    {
        let _guard = enter_scope();
        set_context("request_id", RequestId("req-123".into()));

        handle_request(); // sees "req-123"
    }
    // scope reverted — request_id is back to default
}

fn handle_request() {
    let rid: RequestId = get_context("request_id");
    println!("Handling {}", rid.0);

    {
        let _guard = enter_scope();
        set_context("request_id", RequestId("sub-456".into()));
        do_sub_work(); // sees "sub-456"
    }
    // back to "req-123"
}
```

### 11.2 Cross-Thread Propagation

```rust
use dcontext::{register, enter_scope, set_context, get_context, spawn_with_context};

fn main() {
    register::<RequestId>("request_id");

    let _guard = enter_scope();
    set_context("request_id", RequestId("req-789".into()));

    let handle = spawn_with_context("worker", || {
        let rid: RequestId = get_context("request_id");
        println!("Worker sees: {}", rid.0); // "req-789"
    });

    handle.join().unwrap();
}
```

### 11.3 Async Task Propagation

```rust
use dcontext::{register, enter_scope, set_context, snapshot, spawn_with_context_async};

#[tokio::main]
async fn main() {
    register::<RequestId>("request_id");

    let _guard = enter_scope();
    set_context("request_id", RequestId("req-async".into()));

    let handle = spawn_with_context_async(async {
        let rid: RequestId = dcontext::get_context("request_id");
        println!("Async task sees: {}", rid.0);
    });

    handle.await.unwrap();
}
```

### 11.4 Cross-Process Propagation

```rust
// Sender side (e.g., HTTP client middleware)
let bytes = dcontext::serialize_context().unwrap();
request.headers_mut().insert(
    "x-context",
    dcontext::serialize_context_string().unwrap().parse().unwrap(),
);

// Receiver side (e.g., HTTP server middleware)
let encoded = request.headers().get("x-context").unwrap().to_str().unwrap();
let _guard = dcontext::deserialize_context_string(encoded).unwrap();
// All registered context values are now available via get_context
```

---

## 12. Thread Safety Summary

| Component | Synchronization | Notes |
|-----------|----------------|-------|
| Registry | `RwLock` (global) | Write at startup, read-only at runtime |
| ContextStack | `thread_local! / RefCell` | No cross-thread sharing; each thread has its own |
| ContextSnapshot | `Arc<HashMap>` | Immutable after creation; `Clone + Send + Sync` |
| `get_context` / `set_context` | None (thread-local) | Lock-free on hot path |

---

## 13. Future Work

- **Automatic propagation** via runtime hooks (e.g., Tokio's `tracing` integration).
- **`async-std`** feature implementation.
- **Middleware integrations** for popular web frameworks (axum, actix-web, tonic).
- **Context size limits** to prevent unbounded growth.
- **Metrics** — track context snapshot/restore frequency, serialization overhead.
- **Lazy values** — context entries that are computed on first access.
