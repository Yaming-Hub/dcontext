# dcontext-tracing

Bidirectional bridge between [dcontext](https://crates.io/crates/dcontext) and
[tracing](https://crates.io/crates/tracing) spans.

[![Crates.io](https://img.shields.io/crates/v/dcontext-tracing.svg)](https://crates.io/crates/dcontext-tracing)
[![Docs.rs](https://docs.rs/dcontext-tracing/badge.svg)](https://docs.rs/dcontext-tracing)

This crate copies data between tracing span fields and dcontext context values.
It does **not** manage dcontext scopes — scope lifecycle remains the caller's
responsibility. This keeps the crate simple and avoids issues with span/scope
lifetime mismatches in async code.

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
// Manage scopes yourself
let _scope = dcontext::sync_ctx::enter_named_scope("request");

// Span fields are extracted into context automatically
let _span = tracing::info_span!("handler", request_id = "abc-123").entered();
let id = dcontext::sync_ctx::get_context::<String>("request_id").unwrap();
assert_eq!(id, "abc-123");
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
use dcontext_tracing::TracingField;

let mut builder = dcontext::RegistryBuilder::new();
builder.register_with::<String>("request_id", |opts| {
    opts.with_metadata(
        TracingField::builder("request_id")
            .extract_from_str(|s| Some(s.to_string()))
            .enrich_display::<String>()
            .build(),
    )
});
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
| `TracingField` | Per-key metadata controlling extraction and enrichment |
| `SpanInfo` | Span metadata (name, target, level) |
| `SPAN_INFO_KEY` | Context key for `SpanInfo` (`"dcontext.span"`) |
| `WithContextFields` | Formatter wrapper for log enrichment |
| `collect_log_fields` | Collect all enrichable context values for custom formatters |

## How It Works

The layer hooks into tracing's span lifecycle (`on_new_span`, `on_enter`,
`on_close`) to copy data between spans and the dcontext store. It does
**not** push or pop dcontext scopes — scope management stays with
application code, which can manage scopes independently of span lifetime.

`SyncDcontextLayer` reads/writes `dcontext::sync_ctx` (thread-local).
`AsyncDcontextLayer` reads/writes `dcontext::async_ctx` (task-local) and
performs extraction only on the first enter of each span.

## Related

- [dcontext](https://crates.io/crates/dcontext) — Core context propagation library
- [dcontext-dactor](https://crates.io/crates/dcontext-dactor) — dactor actor framework integration
- [Usage Guide](https://github.com/microsoft/dcontext/blob/main/docs/usage-guide.md)

## License

MIT
