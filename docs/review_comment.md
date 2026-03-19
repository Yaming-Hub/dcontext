# dcontext Design Review Comments

Two independent reviews of `docs/dcontext-design.md` were conducted using
GPT-5.1 and Gemini 3 Pro. This document consolidates their findings.

---

## Critical Issues (🔴)

### C1. Async Fallback Silently Corrupts Context
**Raised by: GPT, Gemini** | §5.2–5.3

The dual-storage dispatch silently falls back to thread-local when no
task-local is established. In async code this is dangerous:

- If a user forgets `spawn_with_context_async` / `with_context`, writes go
  to the **worker thread's** thread-local — lost on task migration, and
  potentially **leaked to unrelated tasks** on the same thread.
- GPT: "This is a correctness landmine for async-heavy code."
- Gemini: "Should panic or return error when running in async context
  without `TASK_CONTEXT` established, rather than silently falling back."

**Recommendation:** Detect async runtime (e.g., `tokio::runtime::Handle::try_current()`)
and panic/error on fallback in async context. Make the "no async without wrapper"
contract explicit and enforced.

---

### C2. ScopeGuard Drop-Order Corruption
**Raised by: Gemini** | §3.2–3.3

`ScopeGuard` calls `leave_scope_internal()` which pops the top of the stack.
If guards are dropped out of order (e.g., `drop(parent_guard)` before child),
the stack becomes corrupted — the wrong scope gets popped.

**Recommendation:** Either:
- Store the expected stack depth / scope ID in `ScopeGuard` and panic on
  mismatch during drop, or
- Promote closure-based `scope(|| ...)` as the primary API to enforce nesting
  at compile time.

---

### C3. RefCell Re-entrancy Panic
**Raised by: Gemini** | §5.1, §2.2

`get_context` borrows `RefCell<ContextStack>` to clone the value. If the
value's `Clone` impl itself calls `get_context`, the `RefCell` will
double-borrow and panic at runtime.

**Recommendation:** Drop the `RefCell` borrow **before** cloning user values.
Extract the `Box<dyn ContextValue>` clone while borrowed, release the borrow,
then downcast.

---

### C4. Thread-Local Mirroring for Sync-in-Async Underspecified
**Raised by: GPT** | §5.2, lines 383–390

The "mirroring task-local into thread-local for sync code within async" is
described conceptually but has no concrete algorithm. Questions remain:

- When exactly does the copy happen?
- How is re-entrancy handled (nested sync calls)?
- Races with multiple tasks on the same worker thread must be proven safe.
- Gemini adds: mirroring is O(N) per scope entry — needs to be lazy or COW.

**Recommendation:** Spell out the exact algorithm, validate against Tokio's
execution model, and consider lazy/COW mirroring.

---

## Important Issues (🟡)

### I1. Panicking API as Default
**Raised by: GPT, Gemini** | §4.1, §7

`get_context`, `set_context`, and `register` all panic on errors (unregistered
key, type mismatch, duplicate registration). This turns configuration mistakes
into runtime crashes.

**Recommendation:**
- Provide `Result`-returning `try_register`, `try_set_context` as primary APIs.
- Keep panicking variants as convenience helpers, clearly documented as
  "development-time assertions only."
- `try_get_context` should also handle "not registered" (return `None`), not
  just "registered but not set."

---

### I2. String Keys — Collision and Performance Risk
**Raised by: GPT, Gemini** | §2.3, §3.1

String keys (`&'static str`) are collision-prone (two libraries using
`"request_id"`) and require allocation/hashing.

**Recommendation:**
- Use `TypeId` as the internal map key for O(1) hashing and collision safety.
- Keep the string key in the registry only for serialization/diagnostics.
- At minimum, change `HashMap<String, …>` to `HashMap<&'static str, …>` to
  avoid cloning `String` on every `set_context`.

---

### I3. Per-Key Versioning for Serialization
**Raised by: GPT, Gemini** | §6.1–6.2

A single `version: u32` on the wire format doesn't handle per-key schema
evolution. If a struct's serde shape changes, old bytes silently deserialize
incorrectly (bincode is not self-describing).

**Recommendation:**
- Add a per-key version field to `WireEntry`.
- Document that users should use backwards-compatible serde changes, or
  recommend a self-describing format (JSON, MessagePack) for the inner value.

---

### I4. Async-std / Runtime-Agnostic Support Incomplete
**Raised by: GPT** | §5.2, §9

`idea.md` requirement #10 asks for flexible async runtime support, but only
Tokio is first-class. `async-std` is listed as "future work."

**Recommendation:** Call this out as a known limitation in docs. Consider the
poll-wrapper (`ContextFuture`) approach discussed earlier as the
runtime-agnostic fallback.

---

### I5. spawn_with_context_async Preconditions
**Raised by: GPT** | §4.5

`spawn_with_context_async` implicitly requires the current thread to have a
valid context (task-local or thread-local). Behavior is unspecified if called
outside `with_context` or from a non-Tokio executor.

**Recommendation:** Document preconditions; return `Result` or fall back to
an empty snapshot with a warning.

---

### I6. Sync-in-Async Mirroring Cost
**Raised by: Gemini** | §5.2

Mirroring the entire task-local into thread-local on scope entry is O(N)
where N = total context items. This could be expensive on every call.

**Recommendation:** Use lazy or copy-on-write mirroring.

---

## Suggestions (🟢)

### S1. Typed Key Wrappers
**Raised by: GPT** | §2.3

A typed-newtype wrapper per key (like `tracing`'s strongly-typed fields)
would eliminate key typos and mismatched `T` at compile time.

---

### S2. Closure-Based Scope API
**Raised by: Gemini** | §4.1, §8.2

`let _ = enter_scope();` is a footgun (immediate drop). Promote
`dcontext::scope(|| ...)` as a primary function alongside the guard API.

---

### S3. Lookup Optimization for Deep Stacks
**Raised by: Gemini** | §3.1

`get_context` walks the stack top-down — O(depth). A flattened COW view
could make reads O(1) at the cost of slightly slower writes. Worth exploring
given reads likely dominate.

---

### S4. Configurable Serialization Codec
**Raised by: GPT** | §6.1

Bincode is fast but not stable across versions/architectures by default.
Consider allowing a pluggable codec for cross-language or long-lived wire
formats (JSON, MessagePack, Protobuf).

---

### S5. Tracing / OpenTelemetry Interop Story
**Raised by: GPT** | §13

Document how `dcontext` interoperates with `tracing::Span` and
`opentelemetry::Context`. Can a `tracing` subscriber read from `dcontext`?
Can OTel baggage be bridged?

---

### S6. Context Size Limits
**Raised by: GPT** | §7

No mechanism to limit context growth. Consider an explicit
`ContextTooLarge` error variant and configurable max size.

---

## Prior Art Comparison Summary

| Library | Relationship to `dcontext` |
|---------|---------------------------|
| `tracing::Span` | Field-based structured logging scopes; robust scope management but not general-purpose key–value. `dcontext` is more flexible. |
| `tokio::task_local!` | `dcontext` adds scope stack + serialization on top of task-local semantics. |
| `opentelemetry::Context` | Very similar goals. OTel contexts are immutable and propagated explicitly (functional style). `dcontext`'s scope stack is more imperative (set and forget). |
| Tower middleware | Uses typed layers/explicit context passing vs `dcontext`'s hidden-global approach. Document trade-offs. |
