# Design Change: Unified Thread-Local Context Store

| Field | Value |
|-------|-------|
| **Status** | Implemented |
| **Author** | dcontext team |
| **Version** | 0.9.0 |
| **Date** | 2026-05-12 |
| **Supersedes** | Dual-context (`sync_ctx` / `async_ctx`) design from 0.8.x |

---

## 1. Motivation

### Current Architecture (0.8.x)

dcontext 0.8.x maintained **two separate context stores**:

| Store | Mechanism | Module |
|-------|-----------|--------|
| Sync context | `thread_local! { CONTEXT: Cell<Option<ContextStore>> }` | `sync_ctx` |
| Async context | `tokio::task_local! { TASK_CONTEXT: Cell<Option<ContextStore>> }` | `async_ctx` |

That design created several problems:

1. **Tokio coupling** — async propagation depended on `tokio::task_local!`, so the core crate was not runtime-agnostic.
2. **Dual-API overhead** — callers had to choose `sync_ctx::*` vs `async_ctx::*` everywhere.
3. **Interop friction** — shared code could not assume a single source of truth for context.
4. **Redundant mechanism** — async task-local propagation can be implemented directly by swapping a store around each future poll.

### Insight

A single `thread_local!` store is enough if async work is wrapped in a future adapter that:

1. owns a `ContextStore`
2. swaps it into thread-local storage on `poll()`
3. polls the inner future
4. swaps the possibly-mutated store back out

That makes thread-local context effectively task-local during a poll without depending on Tokio.

---

## 2. Implemented Design

### 2.1 Single thread-local source of truth

dcontext 0.9 uses one thread-local `ContextStore` as the active context. All sync code reads and writes that store directly.

Async propagation is implemented by `WithContext<F>`, which swaps a store in and out on every poll. This is runtime-agnostic and does not require a Tokio dependency.

### 2.2 Unified public API

All core operations now live at the crate root:

```rust
use dcontext::{
    attach_snapshot, attach_store, capture, clear, fork, get_context_variable,
    merge_with, push_scope, scope_chain, set_context_variable,
    update_context_variable,
};
```

The public free functions are:

- `push_scope(name: &str) -> ScopeGuard`
- `scope_chain() -> Vec<String>`
- `set_context_variable::<T>(key, value)`
- `get_context_variable::<T>(key) -> Option<T>`
- `update_context_variable::<T>(key, f)`
- `capture() -> ContextSnapshot`
- `fork() -> ContextStore`
- `attach_snapshot(snap) -> AttachGuard`
- `attach_store(store) -> AttachGuard`
- `merge_with(source)`
- `clear()`

Example:

```rust
use dcontext::{get_context_variable, push_scope, set_context_variable};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
struct RequestId(String);

let _scope = push_scope("request");
set_context_variable("request_id", RequestId("req-123".into()));

assert_eq!(
    get_context_variable::<RequestId>("request_id"),
    Some(RequestId("req-123".into()))
);
```

### 2.3 Async propagation via `ContextFutureExt`

All `Sized` futures get a blanket extension trait:

- `.with(store)` — use a specific `ContextStore`
- `.attach(snapshot)` — equivalent to `.with(snapshot.into())`
- `.fork()` — fork current context for this future
- `.capture()` — snapshot current context and use that snapshot as a store
- `.scope(name)` — fork current context and push a named scope

Example:

```rust
use dcontext::{ContextFutureExt, get_context_variable, set_context_variable};

let task = async {
    assert_eq!(
        get_context_variable::<String>("request_id"),
        Some("req-123".to_string())
    );
};

set_context_variable("request_id", "req-123".to_string());
let wrapped = task.fork();
```

### 2.4 Store and snapshot semantics

#### `fork()`

`fork()` creates a child `ContextStore` with a frozen parent:

- reads fall through to the frozen parent
- writes are isolated to the child
- local-only values are preserved because the fork stays in-process
- the operation is cheap because values are `Arc`-shared until overwritten

#### `capture()`

`capture()` creates an immutable `ContextSnapshot`:

- values are flattened for transfer or restoration
- local-only values are excluded
- the scope chain is preserved for display and restoration semantics

#### `attach_snapshot()` and `attach_store()`

Both replace the active thread-local context and restore the previous one when the returned guard drops.

#### `merge_with()`

`merge_with()` copies values from another `ContextStore` into the current store without replacing the current scope chain.

---

## 3. Registration and Serialization

### 3.1 Registry configuration

Registrations are built at startup with `RegistryBuilder` and frozen with `initialize(builder)` or `try_initialize(builder)`.

```rust
use dcontext::{initialize, RegistryBuilder};

let mut builder = RegistryBuilder::new();
builder.register::<String>("request_id");
builder.register_with::<u64>("user_id", |opts| opts.cached().version(2));
initialize(builder);
```

Supported registration APIs:

- `register::<T>(key)`
- `register_with::<T>(key, |opts| ...)`
- `register_migration::<Old, New>(key, old_ver, migrate_fn)`

Per-key options are configured with `RegistrationOptions<T>`:

- `.version(...)`
- `.local_only()`
- `.cached()`
- `.codec(encode, decode)`
- `.with_metadata(...)`

### 3.2 Serialization workflow

Outbound:

```rust
let bytes = dcontext::capture().serialize()?;
```

Inbound:

```rust
let snap = dcontext::ContextSnapshot::deserialize(&bytes)?;
let _guard = dcontext::attach_snapshot(snap);
```

`ContextSnapshot::deserialize(bytes)` validates entries against the registry and only restores keys that are currently registered and compatible.

### 3.3 Local-only variables

Local-only variables are a **registration concern**, not a value-trait concern.

A key registered with `.local_only()`:

- is preserved by `fork()`
- is excluded from `capture()` and serialized snapshots
- is filtered out when a snapshot is converted back into a `ContextStore`

---

## 4. Internal Design Notes

### 4.1 `WithContext<F>` implementation

`WithContext<F>` uses `pin-project-lite` for zero-cost pin projection and owns an optional `ContextStore` that is swapped into thread-local storage during `poll()`.

The steady-state overhead is intentionally small: roughly a `Cell` swap on poll (about 4 ns in local measurements).

### 4.2 Send/Sync story

- `ContextStore` is `Send`
- `ContextSnapshot` is `Send + Sync`
- `WithContext<F>` is sendable when `F` is sendable
- `ScopeGuard` and `AttachGuard` are `!Send` and must be dropped on the thread that created them

The value storage is `Arc<dyn ContextValue>` where `ContextValue: Send + Sync`.

### 4.3 Registry DI pattern

Internal logic that depends on registrations accepts `&Registry` instead of reaching directly into global state. This keeps production reads fast while letting tests build isolated registry maps and exercise serialization, capture, filtering, and caching behavior without mutating the global registry.

Examples of this pattern include:

- `wire::serialize_from(&Registry, ...)`
- `wire::deserialize_to_snapshot(&Registry, ...)`
- `ContextStore::push_scope(&Registry, ...)`

---

## 5. Practical Usage Patterns

### 5.1 Spawn local async work

```rust
use dcontext::ContextFutureExt;

tokio::spawn(async move {
    // sees current context
}.fork());
```

### 5.2 Attach inbound remote context

```rust
use dcontext::{attach_snapshot, ContextSnapshot};

fn handle_inbound(bytes: &[u8]) {
    let snap = ContextSnapshot::deserialize(bytes).unwrap();
    let _guard = attach_snapshot(snap);
    // inbound context is now active
}
```

### 5.3 Merge a child store into the current one

```rust
let child = dcontext::fork();
// ... populate child elsewhere ...
dcontext::merge_with(child);
```

---

## 6. Removed Surface from 0.8.x

The following 0.8.x APIs and modules are gone in 0.9:

- `sync_ctx`
- `async_ctx`
- `dcontext-tracing`
- Tokio dependency in the core crate
- `set_raw_value`, `get_raw_value`, `with_context_value`
- `serialize_context()` / `deserialize_context()` free functions
- `push_scope_with_snapshot`
- standalone `register_local`
- `ContextValue::is_local()`

Replacements:

- use crate-root free functions instead of `sync_ctx::*`
- use `ContextFutureExt` instead of `async_ctx::*`
- use `capture().serialize()` / `ContextSnapshot::deserialize(...)`
- use `register_with(key, |o| o.local_only())` for local-only keys
- use `push_scope(...)` plus `merge_with(...)` instead of `push_scope_with_snapshot`

---

## 7. Outcome

dcontext 0.9 ships a single, unified API with one thread-local source of truth, runtime-agnostic async propagation, registry-driven serialization, and isolated testability through dependency-injected `Registry` views.
