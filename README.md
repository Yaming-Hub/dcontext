# dcontext

Distributed context propagation for Rust.

`dcontext` provides a scoped, type-safe key–value store that travels with the
execution flow — across function calls, async/sync boundaries, thread spawns,
and even process boundaries via serialization.

## Features

- **Scoped context** — Enter/leave scopes with automatic rollback (RAII guards)
- **Type-safe** — Compile-time checked access via generics over `Any` storage
- **Cross-thread** — Snapshot and attach context when spawning threads
- **Cross-async** — Helpers for propagating context across Tokio tasks
- **Serializable** — Serialize context to bytes/string for cross-process propagation
- **Runtime-agnostic** — Core is sync-only; async support is opt-in via features

## Quick Start

```toml
[dependencies]
dcontext = "0.1"
```

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
```

## Architecture

```
┌─────────────────────────────────────────┐
│           Application Code              │
├─────────────────────────────────────────┤
│  register / get_context / set_context   │
├─────────────────────────────────────────┤
│  Scope Tree  │  Registry  │  Snapshot   │
├──────────────┼────────────┼─────────────┤
│  Thread-local storage / Task-local      │
└─────────────────────────────────────────┘
```

### Key Concepts

- **Context** — A `HashMap<String, Any>` of named, type-erased values
- **Scope** — A stack frame that overlays the parent; changes revert on exit
- **ScopeGuard** — RAII guard returned by `enter_scope()`; drops → reverts
- **Snapshot** — A captured, cloneable, sendable copy of the current context
- **Registry** — Global type registration for serialization support
- **ContextKey\<T\>** — Optional typed key wrapper for compile-time safe access

## Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `tokio` | yes | Tokio task-local storage, `scope_async`, and async spawn helpers |
| `base64` | yes | Base64 string serialization for HTTP headers/gRPC metadata |
| `context-key` | yes | `ContextKey<T>` typed key wrapper for compile-time safe access |
| `context-future` | no | `ContextFuture` poll-wrapper for runtime-agnostic async (non-Tokio executors) |

## Documentation

See [`docs/dcontext-design.md`](docs/dcontext-design.md) for the full design document.

## License

MIT
