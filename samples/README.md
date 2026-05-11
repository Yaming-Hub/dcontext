# dcontext — Samples

Runnable examples demonstrating typical `dcontext` use cases.
Not included in the published crate (`publish = false`).

## Running

```bash
cargo run --bin <sample_name>
```

## Samples

| Sample | Command | Use Case |
|--------|---------|----------|
| `basic_scope` | `cargo run --bin basic_scope` | Core sync context API — scoped context with automatic revert |
| `cross_thread` | `cargo run --bin cross_thread` | Propagating context across threads via `spawn_with_context` and `wrap_with_context` |
| `async_tasks` | `cargo run --bin async_tasks` | Propagating context across Tokio tasks via `async_ctx::with_context` and `spawn_with_context_async` |
| `feature_flags` | `cargo run --bin feature_flags` | Using context for feature flag propagation with per-request overrides |
| `cross_process` | `cargo run --bin cross_process` | Serializing/deserializing context for cross-process propagation (bytes and base64) |
| `worker_pool` | `cargo run --bin worker_pool` | Dispatching context-aware work items to a pool of worker threads |
| `typed_keys` | `cargo run --bin typed_keys` | `ContextKey<T>` typed wrapper — compile-time type safety without string keys |
| `macros` | `cargo run --bin macros` | `register_contexts!` macro for ergonomic bulk registration |
| `async_scopes` | `cargo run --bin async_scopes` | `async_ctx::scope` for scoped context across `.await` points |
| `size_limits` | `cargo run --bin size_limits` | `set_max_context_size` to cap serialized context size |
| `scope_chain` | `cargo run --bin scope_chain` | Named scopes and distributed scope-chain propagation |
| `tracing_scopes` | `cargo run --bin tracing_scopes` | Tracing integration and automatic scope lifecycle |
| `dual_async_ctx` | `cargo run --bin dual_async_ctx` | Recommended task-local API surface under `dcontext::async_ctx` |
| `dual_sync_ctx` | `cargo run --bin dual_sync_ctx` | Recommended thread-local API surface under `dcontext::sync_ctx` |
| `dual_bridging` | `cargo run --bin dual_bridging` | Bridging snapshots between async and sync code |
| `dual_cross_process` | `cargo run --bin dual_cross_process` | Cross-process propagation with explicit sync/async context selection |
| `dual_tracing_layers` | `cargo run --bin dual_tracing_layers` | `AsyncDcontextLayer` and `SyncDcontextLayer` side by side |
