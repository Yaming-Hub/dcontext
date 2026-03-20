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
| `basic_scope` | `cargo run --bin basic_scope` | Core get/set/scope API — scoped context with automatic revert |
| `cross_thread` | `cargo run --bin cross_thread` | Propagating context across threads via `spawn_with_context` and `wrap_with_context` |
| `async_tasks` | `cargo run --bin async_tasks` | Propagating context across Tokio async tasks via `with_context` and `spawn_with_context_async` |
| `feature_flags` | `cargo run --bin feature_flags` | Using context for feature flag propagation with per-request overrides |
| `cross_process` | `cargo run --bin cross_process` | Serializing/deserializing context for cross-process propagation (bytes and base64) |
| `worker_pool` | `cargo run --bin worker_pool` | Dispatching context-aware work items to a pool of worker threads |
| `typed_keys` | `cargo run --bin typed_keys` | `ContextKey<T>` typed wrapper — compile-time type safety without string keys |
| `macros` | `cargo run --bin macros` | `register_contexts!` and `with_scope!` macros for ergonomic bulk operations |
| `async_scopes` | `cargo run --bin async_scopes` | `scope_async` for scoped context across `.await` points |
| `size_limits` | `cargo run --bin size_limits` | `set_max_context_size` to cap serialized context size |
