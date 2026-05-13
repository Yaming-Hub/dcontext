# Design Change: Unified Thread-Local Context Store

| Field | Value |
|-------|-------|
| **Status** | Proposed |
| **Author** | dcontext team |
| **Version** | 0.9.0 (breaking) |
| **Date** | 2026-05-12 |
| **Supersedes** | Dual-context (sync_ctx / async_ctx) design from 0.8.x |

---

## 1. Motivation

### Current Architecture (0.8.x)

dcontext currently maintains **two separate context stores**:

| Store | Mechanism | Module |
|-------|-----------|--------|
| Sync context | `thread_local! { CONTEXT: Cell<Option<ContextStore>> }` | `sync_ctx` |
| Async context | `tokio::task_local! { TASK_CONTEXT: Cell<Option<ContextStore>> }` | `async_ctx` |

This creates several problems:

1. **Tokio coupling** — The async context requires `tokio::task_local!`, making the core crate depend on tokio. Users on `async-std`, `smol`, `glommio`, or custom executors cannot use async context.

2. **Dual-API cognitive overhead** — Users must choose between `sync_dcontext::*` and `async_dcontext::*` at every call site and understand which store is active in their current execution context.

3. **Redundant mechanism** — `tokio::task_local!` is itself built on `thread_local!`. Tokio wraps the future's poll to set/reset the task-local before/after each poll. We can do the same thing directly, eliminating the middleman.

4. **Interop friction** — Code that runs in both sync and async contexts (e.g., shared libraries, middleware) must handle both stores or pick one, leading to bugs where context is invisible.

### Insight: How OTel Solves This

OpenTelemetry's Rust SDK (`opentelemetry` crate) uses a single `thread_local!` for context storage and propagates context through async code via a **future wrapper** (`WithContext<T>`) that:

1. Captures a context snapshot at wrap time
2. On every `poll()`: pushes captured context onto the thread-local stack
3. Polls the inner future (inner code sees the context via thread-local)
4. On poll return: pops the context (RAII guard drop)

This is **runtime-agnostic** — it works on any async runtime because async executors poll exactly one future per thread at a time. During a poll, the thread-local is effectively task-local.

---

## 2. Proposed Design

### 2.1 Single Thread-Local Store

Replace both `sync_ctx` and `async_ctx` with a **single unified context module** backed by `thread_local!`:

```rust
thread_local! {
    static CONTEXT: Cell<Option<ContextStore>> = Cell::new(Some(ContextStore::new()));
}
```

### 2.2 Unified API Surface

All APIs exported from the dcontext crate root:

```rust
// Core operations (work in both sync and async code)
pub fn push_scope(name: impl Into<String>) -> ScopeGuard { ... }
pub fn set<T: ContextValue>(key: &str, value: T) { ... }
pub fn get<T: ContextValue>(key: &str) -> Option<T> { ... }
pub fn get_or_default<T: ContextValue>(key: &str) -> T { ... }
pub fn scope_chain() -> Vec<String> { ... }
pub fn snapshot() -> ContextSnapshot { ... }

// Fork: create a child store with frozen parent (for local spawn)
pub fn fork() -> ContextStore { ... }

// Attach a snapshot as a new child scope (preserves current scope hierarchy)
pub fn push_scope_with_snapshot(name: &str, snap: ContextSnapshot) -> ScopeGuard { ... }

// Restore a snapshot as root (replaces entire current scope hierarchy)
pub fn attach_snapshot(snap: ContextSnapshot) -> AttachGuard { ... }

// Restore a forked store as root (for local spawn without WithContext wrapper)
pub fn attach_store(store: ContextStore) -> AttachGuard { ... }
```

**`fork()` semantics**:
- Freezes the current scope into an immutable `Arc<ScopeNode>` and returns a new root-level `ContextStore` whose `frozen_parent` points to it.
- Value lookups in the child **fall through** to the frozen parent chain (cheap read inheritance).
- Writes in the child are **isolated** — they go into the child's own scope, not the parent (copy-on-write).
- The frozen parent is `Arc`-shared, so forking is cheap (no deep clone of values).
- Use with `with_context()` for local task spawning.

**`fork` vs `snapshot`**:
- `fork()` → creates a **live-linked** child `ContextStore` (read-through to frozen parent, writes isolated). Cheap. For local spawn.
- `snapshot()` → creates a **fully serializable** `ContextSnapshot` (walks scope chain, clones all values). For wire transfer or cross-process.

**`push_scope_with_snapshot` vs `attach_snapshot`**:
- `push_scope_with_snapshot("name", snap)` — merges the snapshot's **values only** into a new named scope on top of the current hierarchy. The snapshot's scope-chain is **ignored**; the current scope-chain is preserved and the new scope name is appended to it. When the guard drops, the scope is popped back.
- `attach_snapshot(snap)` — replaces the entire current store with the snapshot's flattened values as a new root. The scope hierarchy is **not reconstructed** (values are flattened, scopes cannot be popped), but the snapshot's previous scope names are **preserved for display** in `scope_chain()`. When the guard drops, the previous store is swapped back. Use this at task/request boundaries (e.g., receiving a context from a remote service).

**Typical local spawn patterns**:
```rust
// Preferred: fork_context (fork + wrap in one call)
tokio::spawn(async_work().fork_context());

// Explicit: fork then wrap
let child_store = dcontext::fork();
tokio::spawn(async_work().with_context(child_store));

// Manual: attach_store (when not using the wrapper)
let child_store = dcontext::fork();
tokio::spawn(async move {
    let _guard = dcontext::attach_store(child_store);
    do_work().await;
});
```

### 2.3 Async Propagation via Future Wrapper

```rust
use pin_project_lite::pin_project;

pin_project! {
    pub struct WithContext<F> {
        #[pin]
        inner: F,
        store: ContextStore,  // owned, mutable across polls
    }
}

impl<F: Future> Future for WithContext<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        // Swap our store into thread-local, save whatever was there
        let prev = CONTEXT.with(|c| c.replace(Some(this.store.take())));
        let result = this.inner.poll(cx);
        // Writeback: take (possibly modified) store back, restore prev
        *this.store = CONTEXT.with(|c| c.replace(prev)).unwrap();
        result
    }
}
```

Extension trait for ergonomic use:

```rust
pub trait ContextFutureExt: Sized {
    /// Wrap this future with a specific store (e.g., from fork()).
    fn with_context(self, store: ContextStore) -> WithContext<Self>;
    
    /// Fork the current context and wrap this future with the forked child.
    /// Equivalent to: `self.with_context(dcontext::fork())`
    fn fork_context(self) -> WithContext<Self>;

    /// Snapshot the current context and wrap this future with it.
    /// Equivalent to: `self.with_context(dcontext::snapshot().into_store())`
    /// Use when the future may cross process boundaries or needs a full deep copy.
    fn capture_context(self) -> WithContext<Self>;
}

impl<F: Sized> ContextFutureExt for F {}
```

**`fork_context()` vs `capture_context()`**:
- `fork_context()` — cheap (Arc-shared frozen parent), read-through inheritance. For local spawn.
- `capture_context()` — full deep copy via snapshot. For when the future outlives the parent or needs serialization independence.

This simplifies the common spawn pattern:
```rust
// Local spawn (cheap, inherited reads)
tokio::spawn(async_work().fork_context());

// Independent copy (full snapshot, no parent link)
tokio::spawn(async_work().capture_context());
```

---

## 3. How It Works — Step by Step

### 3.1 Synchronous Code (Unchanged)

```rust
fn handle_request() {
    let _scope = dcontext::push_scope("request");
    dcontext::set("request_id", "abc-123");
    
    do_work();  // can call dcontext::get("request_id") → Some("abc-123")
}
// _scope drops → context restored
```

### 3.2 Async Code — Task Spawning

```rust
async fn handle_request() {
    let _scope = dcontext::push_scope("request");
    dcontext::set("request_id", "abc-123");
    
    // Spawn a task that inherits current context
    let handle = tokio::spawn(
        async_work().with_current_context()
    );
    handle.await.unwrap();
}

async fn async_work() {
    // This works! Thread-local is set by WithContext::poll before we run
    let id = dcontext::get::<String>("request_id");
    assert_eq!(id.as_deref(), Some("abc-123"));
}
```

### 3.3 Cross-Thread Message Passing

```rust
// Sender side: capture snapshot
let snap = dcontext::snapshot();
channel.send(Message { payload, context: snap }).await;

// Receiver side: attach snapshot
async fn process(msg: Message) {
    let _guard = dcontext::push_scope_with_snapshot("incoming", msg.context);
    // context from sender is now active
    do_work();
}
```

### 3.4 Thread Safety Argument

| Property | Guarantee |
|----------|-----------|
| One poll per thread at a time | Async runtimes guarantee this |
| Thread-local isolation | Each thread has its own store |
| `ContextStore` is `Send` | Can be moved across threads (e.g., in `WithContext` or `attach_store`) |
| `ScopeGuard` / `AttachGuard` are `!Send` | Prevents leaking guards across threads |
| `WithContext<F>` is `Send` if `F: Send` | Store moves with the future across threads |
| Nested polls | Swap-in/swap-out handles nesting correctly |

**Key invariant**: Between swap-in and swap-out (within a single `poll()`), no other task can observe this thread's context, because no other task is executing on this thread.

---

## 4. The Swap Pattern: Preserving Mutations Across Await

A critical design choice. Consider:

```rust
async fn work() {
    dcontext::set("step", "1");
    some_io().await;        // yields → poll returns → store swapped out
    // Next poll: store swapped back in
    dcontext::get::<String>("step");  // → Some("1") ✓ (mutations preserved!)
}
```

This works because `WithContext` **owns** the store and swaps it in/out:

```
Poll 1:
  swap_in(self.store)        → thread-local now has our store
  inner.poll()               → sets "step" = "1", returns Pending
  swap_out() → self.store    → our modified store goes back into WithContext

Poll 2:
  swap_in(self.store)        → thread-local has our store again (with "step" = "1")
  inner.poll()               → reads "step" → "1" ✓, returns Ready
  swap_out() → self.store    → done
```

The store **travels with the future**. This is semantically identical to how `tokio::task_local` works internally — tokio swaps task-local storage in/out on each poll of the task's root future.

---

## 5. Migration from 0.8.x

### 5.1 API Mapping

| 0.8.x (sync_ctx) | 0.8.x (async_ctx) | 0.9.0 (unified) |
|-------------------|--------------------|------------------|
| `sync_dcontext::push_scope(n)` | `async_dcontext::push_scope(n)` | `dcontext::push_scope(n)` |
| `sync_dcontext::set(k, v)` | `async_dcontext::set(k, v)` | `dcontext::set(k, v)` |
| `sync_dcontext::get::<T>(k)` | `async_dcontext::get::<T>(k)` | `dcontext::get::<T>(k)` |
| `sync_dcontext::snapshot()` | `async_dcontext::snapshot()` | `dcontext::snapshot()` |
| `sync_ctx::attach(snap)` | `async_ctx::attach(snap)` | `dcontext::push_scope_with_snapshot(name, snap)` |
| N/A | `wrap_with_async_context(f)` | `f.fork_context()` or `f.capture_context()` |
| N/A | `spawn_with_async_context(f)` | `tokio::spawn(f.fork_context())` |

### 5.2 What Gets Removed

- `sync_ctx` module (merged into dcontext root)
- `async_ctx` module (merged into dcontext root)
- `tokio::task_local!` storage
- **Entire tokio dependency** (no longer needed at all)

### 5.3 Breaking Changes

- All call sites change from `sync_dcontext::*` / `async_dcontext::*` to `dcontext::*`
- `wrap_with_async_context(future)` → `future.fork_context()` or `future.capture_context()`
- `spawn_with_async_context(future)` → `tokio::spawn(future.fork_context())`

---

## 6. Design Decisions

### 6.1 Why Owned Store in WithContext (Not Arc/Clone)

OTel uses `Arc<HashMap>` for immutable context, cloning the Arc on every poll. This is cheap but means **mutations within a poll are lost** on the next poll.

dcontext needs mutable context (set/push_scope within async tasks). Therefore `WithContext` **owns** the `ContextStore` and does a swap, not a clone. This:
- Preserves mutations across await points (critical for correctness)
- Avoids allocation (swap is pointer-sized Cell::replace)
- Matches tokio task_local semantics exactly

### 6.2 Guards Are `!Send`, Store Is `Send`

```rust
// ContextStore is Send — can cross thread boundaries
// (values must implement Send via ContextValue trait bound)
unsafe impl Send for ContextStore {}

// Guards are !Send — tied to the thread-local they modify
pub struct AttachGuard {
    prev: Option<ContextStore>,  // saved previous state
    _not_send: PhantomData<*const ()>,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        CONTEXT.with(|c| c.set(self.prev.take()));
    }
}
```

Guards are `!Send` to prevent holding them across await points (which would corrupt another task's context). The `ContextStore` itself is `Send`, allowing it to be moved into `WithContext` wrappers, sent via channels, or passed to `attach_store()` on another thread.



### 6.4 ScopeGuard in Async Code

`ScopeGuard` (from `push_scope`) is `!Send`. Users cannot hold it across `.await`. This is correct:
- In sync code: scope guard lives on the stack, natural RAII
- In async code: use `WithContext` for cross-await propagation, scope guards for within-a-poll scoping only
- For async scope patterns, attach a named snapshot: `dcontext::push_scope_with_snapshot("request", snap)`

---

## 7. Performance Characteristics

### 7.1 Per-Poll Overhead — Identical to `task_local!`

| Operation | Cost |
|-----------|------|
| Swap store into thread-local | 1 × `Cell::replace` (~2 ns) |
| Poll inner future | Normal cost |
| Swap store back | 1 × `Cell::replace` (~2 ns) |
| **Total overhead per poll** | **~4 ns** |

**This is exactly the same overhead as `tokio::task_local!`** because it IS the same mechanism. Tokio's `TaskLocalFuture::poll()` does the same `Cell::replace` swap-in/swap-out on every poll (see §11). We are simply performing the same two `Cell::replace` operations directly, without going through tokio's abstraction layer. There is no additional cost — and no removed cost either. The performance is identical by construction.

### 7.2 No Allocations on Hot Path

- `Cell::replace` is a pointer swap (moves `Option<ContextStore>`)
- No Arc clone needed (store is owned by `WithContext`, not shared)
- Scope push may allocate for scope_chain vec growth (amortized)
- No runtime dispatch, no vtable, no locking

---

## 8. Crate Dependencies After Change

### Core `dcontext` crate:
```toml
[dependencies]
pin-project-lite = "0.2"   # for WithContext pinning (zero-cost, no proc macros)
serde = { version = "1", features = ["derive"], optional = true }

[features]
default = []
serde = ["dep:serde"]

[dev-dependencies]
tokio = { version = "1", features = ["full"] }  # only for tests
```

### Comparison:
| | 0.8.x | 0.9.0 |
|--|-------|-------|
| Core tokio dependency | **Required** | **None** |
| Async support | tokio-only | **Any runtime** |
| New deps | tokio | `pin-project-lite` only (zero-cost) |

---

## 9. Implementation Plan

### Phase 1: Unified Store + Sync API
1. Create single `thread_local!` with `Cell<Option<ContextStore>>`
2. Create `dcontext` crate root with push_scope, set, get, snapshot, push_scope_with_snapshot
3. Implement ScopeGuard and AttachGuard (both `!Send`)
4. Migrate all sync_ctx tests

### Phase 2: WithContext Wrapper
1. Implement `WithContext<F>` with swap-in/swap-out pattern
2. Implement `ContextFutureExt` trait (with_context, with_current_context)
3. Verify mutations persist across await points
4. Migrate all async_ctx tests

### Phase 3: Remove Old Modules  
1. Remove `sync_ctx` and `async_ctx` modules
2. Remove tokio from `[dependencies]` entirely
3. Update public API exports in `lib.rs`

### Phase 4: Dependent Crates & Docs
1. Update `dcontext-tracing` to use unified `dcontext::*` API
2. Update `dcontext-dactor` to use unified `dcontext::*` API  
3. Update design doc, usage guide, README
4. Write migration guide (0.8 → 0.9)

---

## 10. Comparison Summary

| Aspect | 0.8.x (dual store) | 0.9.0 (unified thread-local) |
|--------|---------------------|-------------------------------|
| Storage | thread_local + task_local | thread_local only |
| Runtime dependency | tokio (required) | **None** |
| API surface | 2 modules × N functions | 1 module × N functions |
| Async propagation | tokio::task_local::scope() | WithContext::poll swap |
| Cross-runtime | ❌ No | ✅ Yes (async-std, smol, etc.) |
| Mechanism | Same as tokio internals | Direct (cut out middleman) |
| Per-poll cost | ~same | ~same (both do thread-local swap) |
| Mutations across .await | ✅ Preserved (task_local owns store) | ✅ Preserved (WithContext owns store) |
| Context isolation between tasks | task_local provides | WithContext swap provides |

---

## 11. Appendix: How tokio `task_local!` Works Internally

This section explains the mechanism that `tokio::task_local!` uses under the hood, demonstrating that it is equivalent to the proposed `WithContext` swap pattern.

### 11.1 The Macro Expansion

```rust
tokio::task_local! {
    static MY_VALUE: u32;
}
```

Expands roughly to:

```rust
static MY_VALUE: tokio::task::LocalKey<u32> = tokio::task::LocalKey { /* ... */ };
```

Where `LocalKey<T>` wraps a **`thread_local!`** internally:

```rust
// Simplified from tokio source (tokio/src/task/task_local.rs)
pub struct LocalKey<T: 'static> {
    inner: thread_local::ThreadLocal<RefCell<Option<T>>>,
    //     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    //     This is just std::thread_local! under the hood!
}
```

### 11.2 The `scope()` Method — The Core Mechanism

When you write:

```rust
MY_VALUE.scope(42, async { /* future */ }).await;
```

Tokio creates a wrapper future (`TaskLocalFuture`) that does:

```rust
// Simplified from tokio::task::task_local::TaskLocalFuture
impl<T, F: Future> Future for TaskLocalFuture<T, F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        
        // (1) Swap our value INTO the thread-local slot
        let prev = THREAD_LOCAL.with(|cell| {
            cell.borrow_mut().replace(*this.value.take())
            //                ^^^^^^^ puts our value in, returns what was there
        });
        
        // (2) Poll the inner future — it sees our value via thread-local
        let result = this.future.poll(cx);
        
        // (3) Swap our value BACK OUT, restore previous
        let current = THREAD_LOCAL.with(|cell| {
            cell.borrow_mut().take()  // take our (possibly modified) value out
        });
        *this.value = Some(current);  // save it back into TaskLocalFuture
        
        // Restore previous value
        if let Some(prev) = prev {
            THREAD_LOCAL.with(|cell| cell.borrow_mut().replace(prev));
        }
        
        result
    }
}
```

### 11.3 The `try_with()` / `with()` Accessor

When code inside the future reads the task-local:

```rust
MY_VALUE.with(|val| {
    println!("value = {}", val);
});
```

This simply reads from the **thread-local** slot:

```rust
// Simplified
pub fn with<F, R>(&'static self, f: F) -> R
where F: FnOnce(&T) -> R
{
    THREAD_LOCAL.with(|cell| {
        let borrow = cell.borrow();
        match borrow.as_ref() {
            Some(val) => f(val),
            None => panic!("task-local not set — not inside a .scope() wrapper"),
        }
    })
}
```

### 11.4 Visual Comparison

```
┌─────────────────────────────────────────────────────────────────────┐
│  tokio::task_local! (current dcontext 0.8.x approach)               │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  thread_local! { static SLOT: Cell<Option<T>> }                     │
│                                                                     │
│  TaskLocalFuture::poll():                                           │
│    1. SLOT.set(Some(our_value))     ← swap in                       │
│    2. inner.poll(cx)                ← user code reads SLOT          │
│    3. our_value = SLOT.take()       ← swap out                      │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────┐
│  Proposed dcontext 0.9.0 approach                                   │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  thread_local! { static CONTEXT: Cell<Option<ContextStore>> }       │
│                                                                     │
│  WithContext::poll():                                               │
│    1. CONTEXT.replace(Some(our_store))  ← swap in                   │
│    2. inner.poll(cx)                    ← user code reads CONTEXT   │
│    3. our_store = CONTEXT.replace(prev) ← swap out                  │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘

They are THE SAME PATTERN. The only difference: we skip the tokio
abstraction layer and do the swap directly.
```

### 11.5 Why This Proves the Design Is Sound

| Concern | Answer |
|---------|--------|
| "Is thread-local safe for async?" | Yes — tokio itself uses thread-local for task-locals |
| "What about task switching?" | The swap-out on poll return ensures clean handoff |
| "What about work-stealing?" | Runtime moves the Future (which owns the store), not the thread-local |
| "Mutations across .await?" | Preserved — store is owned by the wrapper, swapped in/out |
| "Nested .scope() calls?" | Previous value is saved and restored (stack semantics) |
| "What if poll panics?" | Same concern as tokio — unwind leaves thread-local in previous state if using RAII guard |

### 11.6 The Redundancy We Eliminate

With dcontext 0.8.x:
```
User code → async_dcontext::set(k,v)
         → TASK_CONTEXT.with(|cell| ...)      [tokio's task_local accessor]
         → thread_local SLOT.with(|cell| ...)  [actual storage]
```

With dcontext 0.9.0:
```
User code → dcontext::set(k,v)
         → CONTEXT.with(|cell| ...)            [actual storage, directly]
```

One fewer indirection layer. Same semantics. No tokio dependency.

---

## 12. References

- [OpenTelemetry Rust Context](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry/src/context.rs) — thread-local + stack approach
- [OpenTelemetry FutureExt](https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry/src/context/future_ext.rs) — WithContext wrapper pattern
- [tokio task_local internals](https://docs.rs/tokio/latest/src/tokio/task/task_local.rs.html) — uses thread_local under the hood
- [pin-project-lite](https://docs.rs/pin-project-lite) — zero-cost pin projection
