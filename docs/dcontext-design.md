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
dcontext::register::<TraceContext>("trace_context");
dcontext::register::<FeatureFlags>("feature_flags");
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
    /// Flattened read cache: merged view of all scopes (copy-on-write).
    /// Invalidated on set_context or enter/leave scope.
    /// None = cache dirty, must rebuild on next read.
    read_cache: Option<HashMap<&'static str, Box<dyn ContextValue>>>,
}
```

The `ContextStack` lives in **thread-local** (sync) or **task-local** (async) storage.

> **Lookup optimization (S3):** Reads are expected to dominate writes. The
> `read_cache` provides O(1) lookups. It is lazily rebuilt on the first read
> after a mutation or scope change, amortizing the merge cost across multiple
> reads.

### 3.2 Scope Lifecycle

| Operation | Effect |
|-----------|--------|
| `enter_scope()` | Pushes a new empty `Scope` onto the stack. Invalidates `read_cache`. Returns a `ScopeGuard`. |
| `leave_scope()` / drop `ScopeGuard` | Validates scope ID, pops the scope, invalidates `read_cache`. |
| `get_context::<T>(key)` | Reads from `read_cache` (rebuilding if dirty), downcasts to `T`. Returns `T` (cloned) or default. |
| `set_context(key, value)` | Inserts/replaces in the current (topmost) scope. Invalidates `read_cache`. |

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
        stack.read_cache = None; // invalidate
    });
}
```

> **Drop-order safety (C2):** Out-of-order drops panic with a clear message,
> preventing silent stack corruption.

### 3.4 Closure-Based Scope API

To enforce correct nesting at compile time (avoiding the `let _ = enter_scope()`
footgun), the library provides a closure-based API as the **recommended primary**:

```rust
/// Execute `f` in a new scope. All context changes in `f` are
/// reverted when it returns. This is the recommended scope API.
pub fn scope<R>(f: impl FnOnce() -> R) -> R {
    let _guard = enter_scope();
    f()
}

/// Async version: execute a future in a new scope.
pub async fn scope_async<F, R>(f: F) -> R
where
    F: Future<Output = R>,
{
    let _guard = enter_scope();
    f.await
}
```

Usage:

```rust
dcontext::scope(|| {
    dcontext::set_context("trace_id", TraceId::new());
    do_work(); // sees the new trace_id
}); // scope automatically reverts

// Guard-based API remains available for cases where closure is awkward:
let _guard = dcontext::enter_scope();
```

---

## 4. Public API

### 4.1 Core Functions

#### Fallible (primary) API

```rust
/// Register a context type. Returns Err if the key is already registered
/// with a different type. Idempotent if called with the same key+type.
pub fn try_register<T>(key: &'static str) -> Result<(), ContextError>
where
    T: Clone + Default + Send + Sync + Serialize + DeserializeOwned + 'static;

/// Get a context value. Returns Ok(Some(T)) if set, Ok(None) if
/// registered but not set, Err if not registered.
pub fn try_get_context<T>(key: &'static str) -> Result<Option<T>, ContextError>
where
    T: Clone + Default + Send + Sync + 'static;

/// Set a context value in the current scope.
/// Returns Err on type mismatch or if key is not registered.
pub fn try_set_context<T>(key: &'static str, value: T) -> Result<(), ContextError>
where
    T: Clone + Send + Sync + 'static;
```

#### Panicking (convenience) API

These are thin wrappers around the `try_` variants. They are intended for
application startup and cases where a missing registration is a programming
error, not a recoverable condition.

```rust
/// Register a context type. Panics if already registered with a different type.
pub fn register<T>(key: &'static str)
where
    T: Clone + Default + Send + Sync + Serialize + DeserializeOwned + 'static;

/// Enter a new scope. Returns a guard that reverts the scope on drop.
pub fn enter_scope() -> ScopeGuard;

/// Get a context value. Returns T::default() if not set.
/// Panics if the key is not registered.
pub fn get_context<T>(key: &'static str) -> T
where
    T: Clone + Default + Send + Sync + 'static;

/// Set a context value in the current scope.
/// Panics if the key is not registered or if T doesn't match.
pub fn set_context<T>(key: &'static str, value: T)
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
///
/// Captures a snapshot of the current context (from task-local or
/// thread-local) and establishes it as a task-local in the spawned task.
///
/// # Preconditions
/// - A Tokio runtime must be active (`tokio::runtime::Handle::current()`).
/// - If no context is currently set (neither task-local nor thread-local),
///   the spawned task starts with an empty context (not an error).
///
/// # Panics
/// Panics if called outside a Tokio runtime.
pub fn spawn_with_context_async<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static;

/// Run an async block with the given snapshot attached as a task-local.
/// Useful for bridging sync → async.
///
/// This establishes the task-local context for the duration of `f`.
/// All `get_context` / `set_context` calls within `f` (and any `.await`-ed
/// sub-futures) will use this task-local context.
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

> **Runtime-agnostic async (I4):** For runtimes without native task-local
> support (async-std, smol, etc.), the library provides `ContextFuture` — a
> **poll-wrapper** that saves/restores thread-local context on each `poll()`.
> Since `poll()` runs on the OS thread currently executing the task, this
> effectively carries context across thread migrations. See `context_future.rs`
> and the `runtime_agnostic` sample.

#### `ContextFuture` poll-wrapper (feature: `context-future`)

For async runtimes that do not provide a built-in task-local mechanism
(async-std, smol, glommio, etc.), the library offers `ContextFuture<F>` — a
`Future` wrapper that manages context via thread-local storage and the
`force_thread_local` escape hatch.

**Poll cycle:**

```
Executor calls ContextFuture::poll()
├── 1. force_thread_local { depth++ }
│   ├── 2. Push snapshot onto thread-local as new scope
│   ├── 3. Poll inner future
│   │   ├── get_context → with_current_stack → depth>0 → thread-local ✓
│   │   ├── .await sub_future → sub_future.poll()
│   │   │   └── get_context → with_current_stack → depth>0 → thread-local ✓
│   │   └── returns Ready(v) or Pending
│   ├── 4. Save mutations back to snapshot
│   └── 5. Pop scope (ScopeGuard drop)
└── force_thread_local { depth-- }
```

**Why inner async functions work without wrappers:**

The key insight is that `force_thread_local` sets a thread-local depth counter
(`FORCE_THREAD_LOCAL_DEPTH`). This counter stays > 0 for the **entire duration**
of a single poll. The dual-storage dispatch (`with_current_stack` — see §5.3)
checks this counter first: if > 0, it skips task-local lookup and routes
directly to thread-local storage. Since the snapshot has been installed in
thread-local, every call to `get_context`/`set_context` during that poll finds
the correct values — regardless of whether the call originates from the async
block itself, a regular async function reached via `.await`, or a deeply nested
sync function.

**Suspension and re-poll:**

When the inner future returns `Pending` (e.g., waiting for I/O):

1. `ContextFuture::poll` captures any mutations back into the snapshot.
2. The scope is popped and `force_thread_local` depth returns to 0.
3. The executor may move the task to a different OS thread.
4. On the next poll, `ContextFuture::poll` re-installs the snapshot on the
   *new* thread's thread-local, increments the depth counter, and polls the
   inner future again. The inner future resumes where it left off, and all
   context calls work correctly.

Mutations made before a yield are preserved because the snapshot is saved
**before** the scope is popped on every poll.

**API:**

```rust
/// Wrap a future with an explicit snapshot.
pub fn ContextFuture::new(snapshot: ContextSnapshot, future: F) -> ContextFuture<F>;

/// Capture the current thread-local context and wrap a future.
pub fn with_context_future<F>(future: F) -> ContextFuture<F>;
```

`with_context_future` uses `force_thread_local` internally to snapshot from
thread-local storage, making it safe to call from any context (including
inside a Tokio runtime where no task-local is established).

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

#### Sync code called from async (mirroring)

When async code calls into synchronous functions that use `get_context`, the
task-local is not directly accessible (sync code cannot `.await`). The library
handles this with a **lazy mirror** protocol:

1. **`with_context` establishes both storages:** When `with_context` sets up
   the task-local, it also installs a **mirror flag** in the thread-local
   indicating "a task-local is active — delegate reads there."
2. **Sync `get_context` checks the flag:** If the mirror flag is set, it
   reads from the task-local via `TASK_CONTEXT.try_with()` (which works in
   sync code — it doesn't require `.await`, only that the task-local was
   previously established on this thread's current task).
3. **No bulk copy:** The mirror is **not** a copy of the data. It's a
   single boolean flag. This makes it O(1) regardless of context size.
4. **Cleanup:** When `with_context`'s scope ends (the future completes or
   is dropped), the mirror flag is cleared.

Since `TASK_CONTEXT.try_with()` works from sync code (it accesses the
task-local that was established by the enclosing `TASK_CONTEXT.scope()` call),
no actual data mirroring is needed — the sync code reads directly from the
task-local.

> **Re-entrancy safety:** Multiple sync calls nested within the same async
> task all read the same task-local. No aliasing occurs because each thread
> runs only one task at a time (Tokio guarantees sequential polling).

### 5.3 Dual-Storage Dispatch

At runtime, `get_context` / `set_context` check which storage backend is
active and dispatch accordingly:

1. If a task-local `TASK_CONTEXT` is set (i.e., we are inside a
   `with_context` scope), use the task-local.
2. If no task-local is set **and** an async runtime is detected
   (`tokio::runtime::Handle::try_current().is_ok()`), **panic** with:
   `"dcontext: get_context/set_context called inside a Tokio runtime without
   with_context/spawn_with_context_async. Context will not survive .await
   points. Wrap your task with dcontext::with_context()."`
3. Otherwise (pure sync, no runtime), fall back to the thread-local `CONTEXT`.

> **Async fallback safety (C1):** The silent fallback to thread-local inside
> an async runtime is eliminated. The panic makes the misuse immediately
> visible during development. For production code that intentionally wants
> thread-local behavior inside a runtime (e.g., `spawn_blocking`), an
> explicit opt-in `force_thread_local(|| ...)` escape hatch is provided.

```rust
fn with_current_stack<R>(f: impl FnOnce(&RefCell<ContextStack>) -> R) -> R {
    // Try task-local first (async path).
    match TASK_CONTEXT.try_with(|stack| f(stack)) {
        Ok(r) => return r,
        Err(_) => {}
    }

    // No task-local. Are we inside an async runtime?
    #[cfg(feature = "tokio")]
    if tokio::runtime::Handle::try_current().is_ok() {
        panic!(
            "dcontext: context accessed inside Tokio runtime without \
             with_context(). Wrap your task with \
             dcontext::spawn_with_context_async() or dcontext::with_context()."
        );
    }

    // Pure sync path — use thread-local.
    CONTEXT.with(|stack| f(stack))
}

/// Escape hatch: explicitly use thread-local storage even inside
/// an async runtime (e.g., inside spawn_blocking).
pub fn force_thread_local<R>(f: impl FnOnce() -> R) -> R {
    FORCE_THREAD_LOCAL.set(true);
    let result = f();
    FORCE_THREAD_LOCAL.set(false);
    result
}
```

### 5.4 RefCell Borrow Safety

To prevent re-entrancy panics when a value's `Clone` impl calls back into
the context API (see C3), all read operations follow this protocol:

```rust
fn get_context_internal<T: Clone + 'static>(type_id: TypeId) -> Option<T> {
    with_current_stack(|cell| {
        // Step 1: Borrow, clone the trait object (cheap pointer copy),
        //         and release the borrow.
        let boxed_clone: Box<dyn ContextValue> = {
            let stack = cell.borrow();
            match stack.lookup(type_id) {
                Some(val) => val.clone_boxed(), // clone_boxed is a shallow clone
                None => return None,
            }
            // RefCell borrow dropped here
        };

        // Step 2: Downcast and clone the inner T (user code runs here,
        //         RefCell is NOT borrowed, so re-entrant calls are safe).
        let any_ref = boxed_clone.as_any();
        Some(any_ref.downcast_ref::<T>().expect("type mismatch").clone())
    })
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
> long-lived wire formats, users can register a custom per-key serializer.
> The default remains bincode for performance; JSON or MessagePack can be
> used for the inner value via the registration API. A top-level codec swap
> (e.g., replace bincode with protobuf for `WireContext` itself) is planned
> as future work.

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

    #[error("no active scope")]
    NoActiveScope,

    #[error("context size exceeds limit: {size} bytes > {limit} bytes")]
    ContextTooLarge { size: usize, limit: usize },
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
| `tokio` | **yes** | Enables Tokio task-local storage, `scope_async`, and async spawn helpers. |
| `base64` | **yes** | Enables `serialize_context_string` / `deserialize_context_string`. |
| `context-key` | **yes** | Enables `ContextKey<T>` typed key wrapper for compile-time safe access. |
| `context-future` | **no** | Enables `ContextFuture` poll-wrapper for runtime-agnostic async (non-Tokio executors). |

> **Runtime-agnostic async (I4):** When the `context-future` feature is enabled,
> `ContextFuture` and `with_context_future` provide a **poll-wrapper** approach
> that works with any async executor (async-std, smol, etc.) without requiring
> runtime-specific APIs. The wrapper saves/restores the thread-local
> `ContextStack` on each `poll()` call, so context follows the task across
> thread migrations. See the `runtime_agnostic` sample.

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
│       ├── typed_keys.rs     ← ContextKey<T> usage
│       ├── macros.rs         ← register_contexts!, with_scope!
│       ├── async_scopes.rs   ← scope_async
│       └── size_limits.rs    ← set_max_context_size
└── dcontext/                 ← core crate
    ├── Cargo.toml
    └── src/
        ├── lib.rs            ← public API re-exports
        ├── registry.rs       ← type registration logic
        ├── scope.rs          ← Scope, ContextStack, ScopeGuard
        ├── storage.rs        ← thread-local + task-local backends, scope_async
        ├── snapshot.rs       ← ContextSnapshot capture/attach
        ├── wire.rs           ← WireContext serialization
        ├── error.rs          ← ContextError
        ├── config.rs         ← set_max_context_size, size limit enforcement
        ├── context_key.rs    ← ContextKey<T> (feature: context-key)
        ├── macros.rs         ← register_contexts!, with_scope!
        ├── helpers.rs        ← spawn_with_context, async helpers
        └── value.rs          ← ContextValue trait + blanket impl
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

### 11.5 Integrating with `tracing` — Span-Scoped Context

A common pattern is tying `dcontext` scopes to `tracing` spans so that
entering a span automatically creates a new context scope, and exiting the
span reverts it. This is achieved with a custom `tracing` `Layer` that hooks
into span lifecycle events.

```rust
use dcontext::{register, enter_scope, set_context, get_context, ScopeGuard};
use serde::{Serialize, Deserialize};
use tracing::{span, Subscriber, Level};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};
use std::sync::Mutex;

// ── Context types ──────────────────────────────────────────────

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TraceContext {
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
}

// ── Custom Layer ───────────────────────────────────────────────

/// A tracing Layer that creates a dcontext scope for each span.
struct DcontextLayer;

/// Per-span storage: holds the ScopeGuard so the scope lives
/// exactly as long as the span is entered.
struct SpanContextGuard(Mutex<Option<ScopeGuard>>);

impl<S> Layer<S> for DcontextLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &span::Attributes<'_>,
        id: &span::Id,
        ctx: Context<'_, S>,
    ) {
        // Optionally extract fields from the span to populate context.
        // Here we just store a placeholder; real code would read
        // attrs.values() or use a visitor.
        if let Some(span) = ctx.span(id) {
            span.extensions_mut()
                .insert(SpanContextGuard(Mutex::new(None)));
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let exts = span.extensions();
            if let Some(guard_holder) = exts.get::<SpanContextGuard>() {
                // Enter a new dcontext scope tied to this span.
                let scope_guard = enter_scope();

                // Populate span-specific context values.
                let parent_trace: TraceContext = get_context("trace_context");
                set_context("trace_context", TraceContext {
                    trace_id: if parent_trace.trace_id.is_empty() {
                        uuid::Uuid::new_v4().to_string()  // root span
                    } else {
                        parent_trace.trace_id              // inherit
                    },
                    span_id: id.into_u64().to_string(),
                    parent_span_id: Some(parent_trace.span_id)
                        .filter(|s| !s.is_empty()),
                });

                *guard_holder.0.lock().unwrap() = Some(scope_guard);
            }
        }
    }

    fn on_exit(&self, id: &span::Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let exts = span.extensions();
            if let Some(guard_holder) = exts.get::<SpanContextGuard>() {
                // Drop the ScopeGuard → reverts context to parent scope.
                let _ = guard_holder.0.lock().unwrap().take();
            }
        }
    }
}

// ── Usage ──────────────────────────────────────────────────────

fn main() {
    // Register context types.
    register::<TraceContext>("trace_context");

    // Install the subscriber with our layer.
    tracing_subscriber::registry()
        .with(DcontextLayer)
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Each span now automatically scopes context.
    let _root = span!(Level::INFO, "handle_request").entered();
    let tc: TraceContext = get_context("trace_context");
    println!("root trace_id={}, span_id={}", tc.trace_id, tc.span_id);

    {
        let _child = span!(Level::INFO, "db_query").entered();
        let tc: TraceContext = get_context("trace_context");
        println!(
            "child trace_id={}, span_id={}, parent={}",
            tc.trace_id,
            tc.span_id,
            tc.parent_span_id.as_deref().unwrap_or("none")
        );
        // trace_id is inherited, span_id is new, parent points to root
    }
    // Back to root span context — child's changes are reverted.
    let tc: TraceContext = get_context("trace_context");
    println!("back to root span_id={}", tc.span_id);
}
```

**How it works:**

1. `DcontextLayer` implements `tracing_subscriber::Layer`.
2. `on_enter` — called when a span is entered — pushes a new `dcontext` scope
   and populates it with trace context (inheriting `trace_id`, generating a
   new `span_id`).
3. `on_exit` — called when the span is exited — drops the `ScopeGuard`,
   reverting context to the parent span's values.
4. The `ScopeGuard` is stored in the span's extensions so its lifetime is
   tied to the span's enter/exit cycle.

This pattern gives you the **best of both worlds**: `tracing`'s structured
span lifecycle with `dcontext`'s arbitrary scoped context propagation — and
the context automatically serializes for cross-process propagation via
`dcontext::serialize_context()`.

### 11.6 Actix-Web Middleware — Request-Scoped Context

This example shows how to use `dcontext` with [actix-web](https://actix.rs/)
to generate a request ID in middleware and make it available to all handlers
and downstream service calls via context, without passing it through function
parameters.

```rust
use actix_web::{web, App, HttpServer, HttpRequest, HttpResponse, middleware};
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use dcontext::{register, get_context, set_context, snapshot, with_context};
use futures::future::{ok, Ready, LocalBoxFuture};
use serde::{Serialize, Deserialize};
use std::task::{Context, Poll};
use uuid::Uuid;

// ── Context types ──────────────────────────────────────────────

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestContext {
    request_id: String,
    user_agent: String,
    path: String,
}

// ── Middleware definition ──────────────────────────────────────

/// Middleware that creates a dcontext scope per request and populates
/// it with a unique request ID and request metadata.
struct DcontextMiddleware;

impl<S, B> Transform<S, ServiceRequest> for DcontextMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = actix_web::Error;
    type Transform = DcontextMiddlewareService<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(DcontextMiddlewareService { service })
    }
}

struct DcontextMiddlewareService<S> {
    service: S,
}

impl<S, B> Service<ServiceRequest> for DcontextMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = actix_web::Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // Extract request metadata before passing ownership.
        let request_id = req
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let user_agent = req
            .headers()
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        let path = req.path().to_string();

        // If the caller sent a serialized context (e.g., from an upstream
        // service), restore it. Otherwise start fresh.
        let snap = if let Some(encoded) = req
            .headers()
            .get("x-context")
            .and_then(|v| v.to_str().ok())
        {
            // Restore upstream context, then overlay request-specific values.
            dcontext::snapshot_from_string(encoded).unwrap_or_default()
        } else {
            dcontext::snapshot()
        };

        let fut = self.service.call(req);

        Box::pin(with_context(snap, async move {
            // Push a request scope on top of any restored upstream context.
            let _guard = dcontext::enter_scope();
            set_context("request_context", RequestContext {
                request_id: request_id.clone(),
                user_agent,
                path,
            });

            let mut res = fut.await?;

            // Echo request ID back in response header.
            res.headers_mut().insert(
                actix_web::http::header::HeaderName::from_static("x-request-id"),
                request_id.parse().unwrap(),
            );

            Ok(res)
        }))
    }
}

// ── Handlers ───────────────────────────────────────────────────

async fn index() -> HttpResponse {
    let ctx: RequestContext = get_context("request_context");
    HttpResponse::Ok().json(serde_json::json!({
        "message": "hello",
        "request_id": ctx.request_id,
    }))
}

async fn users_list() -> HttpResponse {
    let ctx: RequestContext = get_context("request_context");
    tracing::info!(request_id = %ctx.request_id, "listing users");

    // Context is available in any function called from here,
    // no need to pass request_id as a parameter.
    let users = fetch_users_from_db().await;
    HttpResponse::Ok().json(users)
}

async fn fetch_users_from_db() -> Vec<String> {
    // Deep in the call stack — request context is still available.
    let ctx: RequestContext = get_context("request_context");
    tracing::debug!(request_id = %ctx.request_id, "querying database");
    vec!["alice".into(), "bob".into()]
}

// ── Main ───────────────────────────────────────────────────────

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Register context types at startup.
    register::<RequestContext>("request_context");

    HttpServer::new(|| {
        App::new()
            .wrap(DcontextMiddleware)       // ← context middleware
            .wrap(middleware::Logger::default())
            .route("/", web::get().to(index))
            .route("/users", web::get().to(users_list))
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}
```

**How it works:**

1. **`DcontextMiddleware`** wraps each request in a `with_context` future,
   establishing the task-local context for the request's entire lifetime.
2. Inside that scope, it pushes a new `dcontext` scope and populates a
   `RequestContext` with a generated (or forwarded) request ID, the path, and
   user-agent.
3. **Handlers** and any functions they call can access the request context
   via `get_context("request_context")` — no parameter threading needed.
4. For **service-to-service calls**, the upstream context can be forwarded via
   the `x-context` header. The middleware restores it and overlays the
   current request's values on top.
5. The request ID is echoed back in the `x-request-id` response header for
   correlation.

---

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

- **Wire version migration support** — Allow multiple versions of the same
  context type to be registered, each with its own deserializer. When
  deserializing from the wire, the library selects the correct deserializer
  based on the `key_version` in `WireEntry`. This enables rolling upgrades
  where old and new nodes coexist with different struct schemas:
  ```rust
  dcontext::register_versioned::<TraceContextV1>("trace_context", 1);
  dcontext::register_versioned::<TraceContextV2>("trace_context", 2);
  // Wire version 1 → deserializes as V1 then converts to V2
  // Wire version 2 → deserializes as V2 directly
  ```
- **Local-only (non-serializable) context entries** — Allow marking a
  context entry as *local-only* at registration time so that it is excluded
  from `serialize_context()` / `serialize_context_string()`. This is useful
  for entries that contain non-portable data (open file handles, thread IDs,
  in-process caches) or sensitive data (credentials, tokens) that must not
  cross process boundaries:
  ```rust
  dcontext::register_local::<DbConnectionPool>("db_pool");
  // Or with an options builder:
  dcontext::register_with_options::<AuthToken>("auth_token", ContextOptions {
      serialize: false, // excluded from wire format
      ..Default::default()
  });
  ```
  Local-only entries are still propagated via `snapshot()` / `attach()`
  within the same process (e.g., across threads and async tasks), but are
  silently omitted during serialization. This avoids requiring `Serialize`
  bounds on types that are inherently non-serializable.
- **Sample usage programs** — Add a `samples/` directory (excluded from the
  published crate) with runnable examples covering typical use cases:
  request-scoped tracing, feature flags, multi-threaded worker pools,
  async task propagation, and cross-process serialization.
- ~~**`async-std` support** via poll-wrapper `ContextFuture` (see §5.2 / §9).~~ ✅ Implemented as `ContextFuture` / `with_context_future` — runtime-agnostic, works with any executor.
- **Automatic propagation** via runtime hooks (e.g., Tokio's `tracing` integration).
- **Middleware integrations** for popular web frameworks (axum, actix-web, tonic).
- **Context size limits** enforcement (configurable max, `ContextTooLarge` error).
- **Metrics** — track context snapshot/restore frequency, serialization overhead.
- **Lazy values** — context entries that are computed on first access.
- **Pluggable top-level codec** — replace bincode `WireContext` envelope with protobuf/msgpack.
- **`tracing` / OpenTelemetry interop (S5):** Define how `dcontext` relates
  to `tracing::Span` and `opentelemetry::Context`. Possible approaches:
  - A `tracing` subscriber/layer that reads from `dcontext` and attaches
    values as span fields.
  - A bridge that imports OTel baggage into `dcontext` on inbound requests
    and exports `dcontext` values as OTel baggage on outbound calls.
  - `dcontext` is not a replacement for `tracing` — it handles arbitrary
    application context, while `tracing` focuses on structured diagnostics.
    The two are complementary.
