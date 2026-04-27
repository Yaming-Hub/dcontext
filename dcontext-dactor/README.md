# dcontext-dactor

Automatic [dcontext](https://crates.io/crates/dcontext) propagation through
[dactor](https://crates.io/crates/dactor) actor messages.

[![Crates.io](https://img.shields.io/crates/v/dcontext-dactor.svg)](https://crates.io/crates/dcontext-dactor)
[![Docs.rs](https://docs.rs/dcontext-dactor/badge.svg)](https://docs.rs/dcontext-dactor)

When actors send messages to each other — locally or across the network —
distributed context (request IDs, tenant info, feature flags, etc.) needs to
travel with those messages. `dcontext-dactor` makes this transparent by
providing inbound and outbound interceptors that handle serialization,
deserialization, and scope restoration automatically.

## Quick Start

```toml
[dependencies]
dcontext = "0.3"
dcontext-dactor = "0.3"
dactor = "0.3.1"
```

```rust
use dcontext_dactor::{ContextInboundInterceptor, ContextOutboundInterceptor};

// Register interceptors with your dactor runtime
runtime.add_outbound_interceptor(Box::new(ContextOutboundInterceptor::default()));
runtime.add_inbound_interceptor(Box::new(ContextInboundInterceptor::default()));

// That's it! Context now flows automatically between actors.
// Inside any actor handler:
let rid: RequestId = dcontext::get_context("request_id");
```

## How It Works

### Two-Stage Pipeline

The crate uses a **two-stage pipeline** that separates header normalization
from context restoration:

```text
Sender                              Receiver
──────                              ────────
OutboundInterceptor                 InboundInterceptor
  ├─ local target?                    Stage 1: on_receive()
  │   └─ attach ContextSnapshot         ├─ wire header? deserialize → snapshot
  └─ remote target?                     └─ normalize to ContextSnapshotHeader
      └─ serialize → ContextHeader    Stage 2: wrap_handler()
         (includes scope chain)          ├─ enter named scope "remote:<actor_name>"
                                         └─ wrap future with dcontext::with_context()
```

**Outbound** — The `ContextOutboundInterceptor` captures the current context
and attaches it to the message headers. For **local** targets, it uses a
zero-copy snapshot (preserving local-only values). For **remote** targets, it
serializes to wire bytes.

**Inbound** — The `ContextInboundInterceptor` works in two stages:
1. `on_receive` normalizes incoming headers (deserializes wire bytes into a snapshot)
2. `wrap_handler` wraps the handler future with `dcontext::with_context()`, so context
   is automatically available inside the handler — no manual restoration needed.

### Local vs Remote Propagation

| Scenario | Header Type | Serialization | Local-only values |
|----------|-------------|---------------|-------------------|
| Same process | `ContextSnapshotHeader` | None | ✅ Preserved |
| Cross-network | `ContextHeader` | Bincode bytes | ❌ Excluded |

## Error Handling

Both interceptors accept an `ErrorPolicy`:

```rust
use dcontext_dactor::{ContextOutboundInterceptor, ContextInboundInterceptor, ErrorPolicy};

// Log warnings and continue (default) — messages are delivered even if
// context serialization/deserialization fails
let outbound = ContextOutboundInterceptor::default();
let inbound = ContextInboundInterceptor::default();

// Reject — message is dropped if context cannot be propagated
let outbound = ContextOutboundInterceptor::new(ErrorPolicy::Reject);
let inbound = ContextInboundInterceptor::new(ErrorPolicy::Reject);
```

## Wire Transport Registration

If your actors communicate over the network, register the context header
deserializer with dactor's `HeaderRegistry`:

```rust
use dcontext_dactor::register_context_headers;

let mut header_registry = dactor::HeaderRegistry::new();
register_context_headers(&mut header_registry);
// Pass header_registry to your dactor transport configuration
```

## Manually Extracting Context

If you need the propagated context snapshot for spawning sub-tasks or other
manual use, use `extract_context`:

```rust
use dcontext_dactor::extract_context;

async fn my_handler(ctx: &ActorContext, msg: MyMessage) {
    // Get the propagated snapshot (if any)
    if let Some(snapshot) = extract_context(ctx) {
        // Use it to propagate context to a spawned task
        dcontext::spawn_with_context_async(snapshot, async {
            // sub-task has the same context
        }).await;
    }
}
```

## Full Example

See [`samples/src/bin/dactor_propagation.rs`](../samples/src/bin/dactor_propagation.rs)
for a complete working example.

## Scope Chain Integration

The inbound interceptor's `wrap_handler` automatically creates a named scope
`remote:<actor_name>` for each inbound message. This makes distributed call
boundaries visible in the scope chain:

```rust
// Caller (e.g., API gateway) sets up context and sends a message:
let _guard = dcontext::enter_named_scope("api-gateway");
actor_ref.send(MyMessage { ... }).await;

// Inside the receiving actor handler ("OrderActor"):
let chain = dcontext::scope_chain();
// chain == ["api-gateway", "remote:OrderActor"]
```

This gives you a full distributed call path without manual instrumentation.
The scope chain propagates through serialization (wire format v2), so the
chain accumulates as requests traverse multiple services and actors.

## API Reference

| Type | Purpose |
|------|---------|
| `ContextOutboundInterceptor` | Captures context on message send |
| `ContextInboundInterceptor` | Restores context on message receive (two-stage) |
| `ContextHeader` | Serialized wire header (`"dcontext.wire"`) |
| `ContextSnapshotHeader` | Local snapshot header (`"dcontext.snapshot"`) |
| `ErrorPolicy` | `LogAndContinue` (default) or `Reject` |
| `extract_context()` | Manually extract propagated context |
| `register_context_headers()` | Register wire deserializer with `HeaderRegistry` |

## Related

- [dcontext](https://crates.io/crates/dcontext) — Core context propagation library
- [dcontext-tracing](https://crates.io/crates/dcontext-tracing) — Tracing span integration
- [Usage Guide](https://github.com/Yaming-Hub/dcontext/blob/main/docs/usage-guide.md)

## License

MIT
