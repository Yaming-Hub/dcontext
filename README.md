# dcontext

Distributed context propagation for Rust.

`dcontext` provides a scoped, type-safe key–value store that travels with the
execution flow — across function calls, async/sync boundaries, thread spawns,
and even process boundaries via serialization.

## Features

- **Scoped context** — Enter/leave scopes with automatic rollback (RAII guards)
- **Type-safe** — Compile-time checked access via generics over `Any` storage
- **Unified API** — Single crate-root API for both sync and async code
- **Runtime-agnostic** — No Tokio dependency; works with any async executor
- **Cross-thread** — Fork context and attach in spawned threads
- **Cross-async** — `ContextFutureExt` trait (`.fork()`, `.attach()`, `.capture()`, `.scope()`)
- **Serializable** — Serialize context to bytes for cross-process propagation
- **Local-only entries** — Non-serializable context that stays within the process
- **Version migration** — Rolling upgrades with automatic old→new schema conversion
- **Scope chain** — Named scopes with queryable distributed call chain
- **O(1) cached reads** — Per-key caching option for frequently-read values

## Quick Start

```toml
[dependencies]
dcontext = "0.9"
```

```rust
use dcontext::{
    initialize, push_scope, set_context_variable, get_context_variable,
    capture, ContextFutureExt, RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[tokio::main]
async fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    initialize(builder);

    // Sync usage
    {
        let _scope = push_scope("ingress");
        set_context_variable("request_id", RequestId("req-123".into()));

        let rid = get_context_variable::<RequestId>("request_id").unwrap();
        let chain = dcontext::scope_chain();
        println!("sync: {:?} {:?}", rid, chain);
    }

    // Async usage — fork context into spawned task
    set_context_variable("request_id", RequestId("req-456".into()));

    tokio::spawn(async {
        let rid = get_context_variable::<RequestId>("request_id").unwrap();
        println!("async task sees: {:?}", rid);
    }.fork()).await.unwrap();

    // Cross-process serialization
    let bytes = capture().serialize().unwrap();
    let snap = dcontext::ContextSnapshot::deserialize(&bytes).unwrap();
    let _guard = dcontext::attach_snapshot(snap);
}
```

## Architecture

```
┌─────────────────────────────────────────────┐
│           Application Code                  │
├─────────────────────────────────────────────┤
│ RegistryBuilder / initialize                │
│ push_scope / set / get / capture / fork     │
│ ContextFutureExt: .fork() .attach() .scope()│
├─────────────────────────────────────────────┤
│         WithContext<F> Future Wrapper        │
│   (swaps ContextStore in/out on each poll)  │
├─────────────────────────────────────────────┤
│  Scope Tree  │  Registry  │  Snapshot       │
├──────────────┼────────────┼─────────────────┤
│       std::thread_local! { Cell<...> }      │
└─────────────────────────────────────────────┘
```

### Key Concepts

- **ContextStore** — The mutable context state (scope stack + values). Lives in thread-local.
- **Scope** — A stack frame that overlays the parent; changes revert on exit
- **ScopeGuard** — RAII guard from `push_scope()`; drops → reverts
- **Scope Chain** — Ordered list of scope names; query with `scope_chain()`
- **ContextSnapshot** — Immutable, serializable copy of the current context (Send + Sync)
- **WithContext\<F\>** — Future wrapper that makes thread-local context task-local per poll
- **ContextFutureExt** — Extension trait: `.with()`, `.fork()`, `.attach()`, `.capture()`, `.scope()`
- **Registry** — Global type registration (frozen after `initialize()`, lock-free reads)
- **ContextKey\<T\>** — Optional typed key wrapper for compile-time safe access

## Integration Crates

| Crate | Description |
|-------|-------------|
| [dcontext-dactor](dcontext-dactor/) | Automatic context propagation through [dactor](https://crates.io/crates/dactor) actor messages |

## Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `base64` | yes | Base64 string serialization for HTTP headers/gRPC metadata |
| `context-key` | yes | `ContextKey<T>` typed key wrapper for compile-time safe access |

## Documentation

- **[Usage Guide](docs/usage-guide.md)** — Comprehensive guide covering all features with examples
- **[Design Document](docs/design.md)** — Internal architecture and design decisions

## Samples

Run any sample with `cargo run --bin <name>`:

| Sample | Description |
|--------|-------------|
| `basic_scope` | Core sync context API and scoped rollback |
| `cross_thread` | Thread propagation via fork + attach_store |
| `async_tasks` | Async propagation via ContextFutureExt |
| `async_scopes` | Named scopes across `.await` points |
| `cross_process` | Serialization (bytes/base64) |
| `typed_keys` | `ContextKey<T>` type safety |
| `macros` | `register_contexts!` macro |
| `worker_pool` | Context-aware work dispatch |
| `feature_flags` | Per-request feature flag overrides |
| `size_limits` | `set_max_context_size` cap |
| `scope_chain` | Named scopes and scope chain query |
| `custom_codec` | Custom per-key serialization codec |
| `version_migration` | Wire version migration between schema versions |
| `dactor_propagation` | dcontext-dactor propagation flow |

## License

MIT
