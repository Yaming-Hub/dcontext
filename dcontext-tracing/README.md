# dcontext-tracing

Automatic [dcontext](https://crates.io/crates/dcontext) scope management via
[tracing](https://crates.io/crates/tracing) spans.

[![Crates.io](https://img.shields.io/crates/v/dcontext-tracing.svg)](https://crates.io/crates/dcontext-tracing)
[![Docs.rs](https://docs.rs/dcontext-tracing/badge.svg)](https://docs.rs/dcontext-tracing)

When you enter a tracing span, this crate automatically creates a dcontext
scope. When the span exits, the scope is reverted. This means your context
values follow the natural span lifecycle — no manual scope management needed.

## Quick Start

```toml
[dependencies]
dcontext = "0.8"
dcontext-tracing = "0.8"
tracing = "0.1"
tracing-subscriber = "0.3"
```

```rust
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(dcontext_tracing::SyncDcontextLayer::new())
    .init();
```

## Two Recommended Integration Modes

### Sync / Thread-Local Code

Use `SyncDcontextLayer` with `dcontext::sync_ctx`:

```rust
dcontext::sync_ctx::set_context("user", "alice".to_string());

{
    let _span = tracing::info_span!("request").entered();
    dcontext::sync_ctx::set_context("request_id", "abc-123".to_string());

    let user = dcontext::sync_ctx::get_context::<String>("user").unwrap();
    assert_eq!(user, "alice");
}
// Scope reverted — "request_id" is gone, "user" remains
```

### Async / Task-Local Code

Use `AsyncDcontextLayer` with `dcontext::async_ctx`:

```rust
use dcontext::async_ctx;
use dcontext_tracing::AsyncDcontextLayer;
use tracing::Instrument;
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(AsyncDcontextLayer::new())
    .init();

async fn handle_request() {
    let id = async_ctx::get_context::<RequestId>("request_id").unwrap();
    assert_eq!(id.0, "req-001");
}

async_ctx::with_context(dcontext::ContextSnapshot::empty(), async {
    handle_request()
        .instrument(tracing::info_span!("handler", request_id = "req-001"))
        .await;
})
.await;
```

## Field-to-Context Mapping

Map tracing span fields directly to dcontext values. When a span with the
configured field is entered, the value is extracted and set in context:

```rust
use dcontext::sync_ctx;
use dcontext_tracing::{FromFieldValue, SyncDcontextLayer};

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
struct RequestId(String);

impl FromFieldValue for RequestId {
    fn from_str_value(s: &str) -> Option<Self> {
        Some(RequestId(s.to_string()))
    }
}

let layer = SyncDcontextLayer::builder()
    .map_field::<RequestId>("request_id")
    .build();

tracing_subscriber::registry().with(layer).init();

let _span = tracing::info_span!("handler", request_id = "req-001").entered();
let id = sync_ctx::get_context::<RequestId>("request_id").unwrap();
assert_eq!(id.0, "req-001");
```

You can also map a field to a different context key name:

```rust
let layer = dcontext_tracing::SyncDcontextLayer::builder()
    .map_field_as::<RequestId>("req_id", "request_id")
    .build();
```

## Span Info

Expose span metadata (name, target, level) as a context value:

```rust
use dcontext::sync_ctx;
use dcontext_tracing::{SpanInfo, SyncDcontextLayer, SPAN_INFO_KEY};

let layer = SyncDcontextLayer::builder()
    .include_span_info()
    .build();

tracing_subscriber::registry().with(layer).init();

{
    let _span = tracing::info_span!("process_order").entered();
    let info = sync_ctx::get_context::<SpanInfo>(SPAN_INFO_KEY).unwrap();
    println!("Span: {} ({})", info.name, info.level);
}
```

## Full Example

See [`samples/src/bin/tracing_scopes.rs`](../samples/src/bin/tracing_scopes.rs)
for a complete working example.

## API Reference

| Type | Purpose |
|------|---------|
| `SyncDcontextLayer<S>` | Tracing layer for thread-local `dcontext::sync_ctx` |
| `AsyncDcontextLayer<S>` | Tracing layer for task-local `dcontext::async_ctx` |
| `DcontextLayer<S>` | Legacy alias retained for compatibility |
| `FromFieldValue` | Trait for converting tracing fields to context types |
| `SpanInfo` | Span metadata (name, target, level) |
| `SPAN_INFO_KEY` | Context key for `SpanInfo` (`"dcontext.span"`) |

## How It Works

`SyncDcontextLayer` (and the legacy `DcontextLayer` alias) use a
**thread-local stack** to store dcontext `ScopeGuard`s (which are `!Send` and
cannot be stored in tracing's span extensions). On span enter, a new scope is
pushed; on span exit, the scope is popped and the guard dropped, reverting
context changes.

`AsyncDcontextLayer` writes to `dcontext::async_ctx` task-local storage instead
and keeps per-span lifecycle state in span extensions so scopes survive
Tokio yield points and task migration.

## Scope Chain Integration

Span names automatically become named scopes in the dcontext scope chain.
Each time a span is entered, the layer pushes the span name into the active
store, so scope-chain queries reflect the tracing span hierarchy:

```rust
let _outer = tracing::info_span!("api_handler").entered();
{
    let _inner = tracing::info_span!("db_query").entered();
    let chain = dcontext::sync_ctx::scope_chain();
    // chain == ["api_handler", "db_query"]
}
// After _inner exits: chain == ["api_handler"]
```

With `AsyncDcontextLayer`, use `dcontext::async_ctx::scope_chain()` instead so
the chain follows the Tokio task across `.await` points.

## Related

- [dcontext](https://crates.io/crates/dcontext) — Core context propagation library
- [dcontext-dactor](https://crates.io/crates/dcontext-dactor) — dactor actor framework integration
- [Usage Guide](https://github.com/microsoft/dcontext/blob/main/docs/usage-guide.md)

## License

MIT
