# dcontext

Distributed context propagation for Rust.

`dcontext` provides a scoped, type-safe key–value store that travels with the
execution flow — across function calls, async/sync boundaries, thread spawns,
and even process boundaries via serialization.

## Features

- **Scoped context** — Enter/leave scopes with automatic rollback (RAII guards)
- **Type-safe** — Compile-time checked access via generics over `Any` storage
- **Dual-context model** — Use `dcontext::sync_ctx` for thread-local code and `dcontext::async_ctx` for task-local code
- **Cross-thread** — Snapshot and attach context when spawning threads
- **Cross-async** — Helpers for propagating context across Tokio tasks
- **Serializable** — Serialize context to bytes/string for cross-process propagation
- **Local-only entries** — Non-serializable context that stays within the process
- **Version migration** — Rolling upgrades with automatic old→new schema conversion
- **Scope chain** — Named scopes with queryable distributed call chain (`sync_ctx::scope_chain()` / `async_ctx::scope_chain()`)
- **O(1) lookups** — Index-based read cache for fast context access

## Quick Start

```toml
[dependencies]
dcontext = "0.8"
```

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

    {
        let _guard = sync_ctx::enter_named_scope("ingress");
        sync_ctx::set_context("request_id", RequestId("req-123".into()));

        let rid = sync_ctx::get_context::<RequestId>("request_id").unwrap();
        let chain = sync_ctx::scope_chain();
        println!("sync: {:?} {:?}", rid, chain);
    }

    let snap = {
        let _guard = sync_ctx::enter_scope();
        sync_ctx::set_context("request_id", RequestId("req-456".into()));
        sync_ctx::snapshot()
    };

    async_ctx::with_context(snap, async {
        async_ctx::scope("handler", async {
            let rid = async_ctx::get_context::<RequestId>("request_id").unwrap();
            let chain = async_ctx::scope_chain();
            println!("async: {:?} {:?}", rid, chain);
        })
        .await;
    })
    .await;
}
```

## Architecture

```
┌─────────────────────────────────────────────┐
│           Application Code                  │
├─────────────────────────────────────────────┤
│ RegistryBuilder / initialize                │
│ sync_ctx::*  or  async_ctx::*               │
├───────────────────────┬─────────────────────┤
│  dcontext::async_ctx  │  dcontext::sync_ctx │
│  (Task-Local Store)   │  (Thread-Local)     │
├───────────────────────┼─────────────────────┤
│  AsyncDcontextLayer   │  SyncDcontextLayer  │
│  (tracing spans)      │  (tracing spans)    │
├───────────────────────┴─────────────────────┤
│  Scope Tree  │  Registry  │  Snapshot       │
├──────────────┼────────────┼─────────────────┤
│  tokio task_local!  /  std thread_local!    │
└─────────────────────────────────────────────┘
```

### Key Concepts

- **Context** — A `HashMap<String, Any>` of named, type-erased values
- **Scope** — A stack frame that overlays the parent; changes revert on exit
- **ScopeGuard** — RAII guard returned by `sync_ctx::enter_scope()` / `sync_ctx::enter_named_scope()`; drops → reverts
- **Scope Chain** — Ordered list of named scope names (local + remote prefix); query with `sync_ctx::scope_chain()` or `async_ctx::scope_chain()`
- **Snapshot** — A captured, cloneable, sendable copy of the current context
- **Registry** — Global type registration for serialization support
- **ContextKey<T>** — Optional typed key wrapper for compile-time safe access

## Integration Crates

| Crate | Description |
|-------|-------------|
| [dcontext-dactor](dcontext-dactor/) | Automatic context propagation through [dactor](https://crates.io/crates/dactor) actor messages |
| [dcontext-tracing](dcontext-tracing/) | Automatic context scoping via [tracing](https://crates.io/crates/tracing) spans |

## Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `tokio` | yes | Tokio task-local storage, `async_ctx::scope`, and async propagation helpers |
| `base64` | yes | Base64 string serialization for HTTP headers/gRPC metadata |
| `context-key` | yes | `ContextKey<T>` typed key wrapper for compile-time safe access |

## Documentation

- **[Usage Guide](docs/usage-guide.md)** — Comprehensive guide covering all features with examples
- **[Design Document](docs/dcontext-design.md)** — Internal architecture and design decisions

## Samples

Run any sample with `cargo run --bin <name>`:

| Sample | Description |
|--------|-------------|
| `basic_scope` | Core sync context API and scoped rollback |
| `cross_thread` | Thread propagation via `spawn_with_context` |
| `async_tasks` | Tokio propagation via `async_ctx::with_context` and `spawn_with_context_async` |
| `async_scopes` | `async_ctx::scope` across `.await` points |
| `cross_process` | Serialization (bytes/base64) |
| `typed_keys` | `ContextKey<T>` type safety |
| `macros` | `register_contexts!` macro |
| `worker_pool` | Context-aware work dispatch |
| `feature_flags` | Per-request feature flag overrides |
| `size_limits` | `set_max_context_size` cap |
| `tracing_scopes` | dcontext-tracing integration |
| `scope_chain` | Named scopes and scope chain query |
| `dactor_propagation` | dcontext-dactor propagation flow |
| `dual_async_ctx` | `async_ctx` module — task-local context |
| `dual_sync_ctx` | `sync_ctx` module — thread-local context |
| `dual_bridging` | Async→sync snapshot bridging patterns |
| `dual_cross_process` | Serialize context and restore remotely |
| `dual_tracing_layers` | `AsyncDcontextLayer` + `SyncDcontextLayer` |

## License

MIT
