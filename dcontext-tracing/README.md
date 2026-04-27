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
dcontext = "0.2"
dcontext-tracing = "0.2"
tracing = "0.1"
tracing-subscriber = "0.3"
```

```rust
use tracing_subscriber::prelude::*;

// Zero-config: every span creates a dcontext scope
tracing_subscriber::registry()
    .with(dcontext_tracing::DcontextLayer::new())
    .init();
```

## Three Levels of Integration

### Level 1: Automatic Scoping (Zero Config)

Every span enter creates a new dcontext scope that inherits the parent
scope's values. When the span exits, changes are reverted:

```rust
dcontext::set_context("user", "alice".to_string());

{
    let _span = tracing::info_span!("request").entered();
    // New scope — inherits "user" = "alice"
    dcontext::set_context("request_id", "abc-123".to_string());

    let user: String = dcontext::get_context("user");
    assert_eq!(user, "alice"); // inherited from parent
}
// Scope reverted — "request_id" is gone, "user" remains
```

### Level 2: Field-to-Context Mapping

Map tracing span fields directly to dcontext values. When a span with the
configured field is entered, the value is extracted and set in context:

```rust
use dcontext_tracing::{DcontextLayer, FromFieldValue};

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
struct RequestId(String);

impl FromFieldValue for RequestId {
    fn from_str_value(s: &str) -> Option<Self> {
        Some(RequestId(s.to_string()))
    }
}

let layer = DcontextLayer::builder()
    .map_field::<RequestId>("request_id")
    .build();

tracing_subscriber::registry().with(layer).init();

// Now this span automatically sets RequestId in dcontext:
let _span = tracing::info_span!("handler", request_id = "req-001").entered();
let id: RequestId = dcontext::get_context("request_id");
assert_eq!(id.0, "req-001");
```

You can also map a field to a different context key name:

```rust
let layer = DcontextLayer::builder()
    .map_field_as::<RequestId>("req_id", "request_id")
    .build();
```

### Level 3: Span Info

Expose span metadata (name, target, level) as a context value:

```rust
use dcontext_tracing::{DcontextLayer, SpanInfo, SPAN_INFO_KEY};

let layer = DcontextLayer::builder()
    .include_span_info()
    .build();

tracing_subscriber::registry().with(layer).init();

{
    let _span = tracing::info_span!("process_order").entered();
    let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
    println!("Span: {} ({})", info.name, info.level);
    // Output: Span: process_order (INFO)
}
```

### Combining All Levels

```rust
let layer = DcontextLayer::builder()
    .map_field::<RequestId>("request_id")
    .map_field::<TenantId>("tenant_id")
    .include_span_info()
    .build();
```

## Implementing `FromFieldValue`

The `FromFieldValue` trait converts tracing field values to your context types.
Implement the conversion methods that match your field's type:

```rust
use dcontext_tracing::FromFieldValue;

// String field → context type
#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
struct TraceId(String);

impl FromFieldValue for TraceId {
    fn from_str_value(s: &str) -> Option<Self> {
        Some(TraceId(s.to_string()))
    }
}

// Numeric field → context type
#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
struct RetryCount(u64);

impl FromFieldValue for RetryCount {
    fn from_u64_value(v: u64) -> Option<Self> {
        Some(RetryCount(v))
    }
}

// Boolean field → context type
#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
struct IsAdmin(bool);

impl FromFieldValue for IsAdmin {
    fn from_bool_value(v: bool) -> Option<Self> {
        Some(IsAdmin(v))
    }
}
```

## Async Behavior

When used with [`Instrument`](https://docs.rs/tracing/latest/tracing/trait.Instrument.html),
the layer creates and reverts a scope around each poll of the future. Mapped
field values and span info are re-applied on each enter, so reads via
`force_thread_local()` will see the correct values during each poll:

```rust
use tracing::Instrument;

async fn handle_request() {
    // Inside the span, read context via force_thread_local
    let id: RequestId = dcontext::force_thread_local(|| {
        dcontext::get_context("request_id")
    });
}

handle_request()
    .instrument(tracing::info_span!("handler", request_id = "req-001"))
    .await;
```

> **Note:** Mutations made inside a span do **not** persist across `.await`
> points — each poll gets a fresh scope. For full async context propagation
> across `.await`, use `dcontext::with_context()` or `dcontext::ContextFuture`
> directly.

## Full Example

See [`samples/src/bin/tracing_scopes.rs`](../samples/src/bin/tracing_scopes.rs)
for a complete working example.

## API Reference

| Type | Purpose |
|------|---------|
| `DcontextLayer<S>` | Tracing layer — creates dcontext scopes on span enter/exit |
| `DcontextLayerBuilder<S>` | Builder for configuring the layer |
| `FromFieldValue` | Trait for converting tracing fields to context types |
| `SpanInfo` | Span metadata (name, target, level) |
| `SPAN_INFO_KEY` | Context key for `SpanInfo` (`"dcontext.span"`) |

## How It Works

The layer uses a **thread-local stack** to store dcontext `ScopeGuard`s (which
are `!Send` and cannot be stored in tracing's span extensions). On span enter,
a new scope is pushed; on span exit, the scope is popped and the guard dropped,
reverting context changes. This mirrors the approach used by
`tracing-opentelemetry`.

All dcontext operations in callbacks use `force_thread_local()` to ensure
correct behavior inside tokio runtimes.

## Related

- [dcontext](https://crates.io/crates/dcontext) — Core context propagation library
- [dcontext-dactor](https://crates.io/crates/dcontext-dactor) — dactor actor framework integration
- [Usage Guide](https://github.com/Yaming-Hub/dcontext/blob/main/docs/usage-guide.md)

## License

MIT
