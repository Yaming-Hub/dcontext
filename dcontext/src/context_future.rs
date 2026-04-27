use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::snapshot::ContextSnapshot;
use crate::storage;

/// A `Future` wrapper that carries a [`ContextSnapshot`] and installs it into
/// thread-local storage on every `poll()`. This is the **runtime-agnostic**
/// mechanism for propagating context across async boundaries — it works with
/// any executor (Tokio, async-std, smol, etc.) because it relies only on
/// thread-local storage, not runtime-specific task-locals.
///
/// # How it works
///
/// On each `poll()`:
/// 1. The captured snapshot is pushed onto the thread-local context stack as a
///    new scope, using [`force_thread_local`](crate::force_thread_local) to
///    bypass any Tokio task-local dispatch.
/// 2. The inner future is polled. Because `force_thread_local` sets a
///    thread-local depth counter (`FORCE_THREAD_LOCAL_DEPTH > 0`), **all** code
///    executed during this poll — including `get_context`, `set_context`, and
///    any regular async functions reached via `.await` — is routed to thread-local
///    storage automatically. No special wrappers are needed in inner code.
/// 3. Any mutations made during polling are saved back to the snapshot so that
///    state persists across `.await` suspension points.
/// 4. The pushed scope is popped, restoring the thread-local to its prior state.
///
/// Because `poll()` always runs on the OS thread currently executing the task,
/// and we set up / tear down thread-local around each poll, context effectively
/// follows the task across thread migrations.
///
/// # Why inner async functions just work
///
/// A common concern is whether regular async functions (returning plain `Future`,
/// not `ContextFuture`) will see the context when `.await`ed inside a
/// `ContextFuture`. The answer is **yes** — here's why:
///
/// When `ContextFuture::poll()` is called by the executor, it calls
/// `force_thread_local(|| { ... })`, which increments the thread-local
/// `FORCE_THREAD_LOCAL_DEPTH` counter. This counter stays > 0 for the entire
/// duration of the poll. The context dispatch function (`with_current_stack`)
/// checks this counter first — if > 0, it skips task-local lookup and goes
/// straight to thread-local storage. Since the snapshot has been installed in
/// thread-local, all `get_context`/`set_context` calls during the poll will
/// find the correct values.
///
/// When the inner future `.await`s a sub-future that returns `Pending`:
/// 1. The sub-future's `poll` returns `Pending`.
/// 2. The async block (inner future) also returns `Pending`.
/// 3. `ContextFuture::poll` saves mutations back to the snapshot and pops the
///    scope. The depth counter goes back to 0.
/// 4. On the next poll (possibly on a different thread), `ContextFuture::poll`
///    repeats the whole setup — re-installs snapshot, increments depth, polls
///    the inner future, which resumes where it left off.
///
/// This means context is correctly propagated regardless of how many `.await`
/// points exist, how many regular futures are chained, or how many times the
/// task migrates between threads.
///
/// # Comparison with Tokio `with_context`
///
/// | | `with_context` (Tokio) | `ContextFuture` (any runtime) |
/// |---|---|---|
/// | **Runtime** | Tokio only | Any executor |
/// | **Mechanism** | `tokio::task_local!` | Thread-local + poll-wrapper |
/// | **Feature flag** | `tokio` | `context-future` |
/// | **Inner code needs wrappers?** | No | No |
/// | **Overhead per poll** | None (task-local is persistent) | O(N) snapshot install/teardown |
///
/// # Example
///
/// ```rust,ignore
/// use dcontext::{register, set_context, get_context, with_context_future};
///
/// register::<TraceId>("trace_id");
/// set_context("trace_id", TraceId("abc".into()));
///
/// // Wrap the top-level future — all inner .await chains see context
/// let fut = with_context_future(async {
///     // Direct access — no force_thread_local needed
///     let t: TraceId = get_context("trace_id");
///
///     // Regular async functions also see context automatically
///     let result = some_regular_async_fn().await;
/// });
/// ```
pub struct ContextFuture<F> {
    inner: F,
    /// Mutable snapshot — mutations during poll are captured back.
    snapshot: ContextSnapshot,
}

impl<F> ContextFuture<F>
where
    F: Future,
{
    /// Create a new `ContextFuture` wrapping the given future with a snapshot.
    pub fn new(snapshot: ContextSnapshot, future: F) -> Self {
        Self {
            inner: future,
            snapshot,
        }
    }
}

// SAFETY: ContextFuture is Send if the inner future is Send.
// The snapshot is always Send (it uses Arc<HashMap<...>> of Send values).
unsafe impl<F: Send> Send for ContextFuture<F> {}

impl<F> Future for ContextFuture<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: We only move `inner` through Pin projection. `snapshot` is
        // not structurally pinned so we can freely access it.
        let this = unsafe { self.get_unchecked_mut() };

        // 1. Push the snapshot values onto thread-local as a new scope.
        //    Use force_thread_local to ensure we go to thread-local even
        //    if a task-local happens to exist (we manage our own state).
        let result = storage::force_thread_local(|| {
            let _guard = install_snapshot(&this.snapshot);

            // 2. Poll the inner future.
            // SAFETY: We are re-pinning `inner` which was already pinned via `self`.
            let pinned = unsafe { Pin::new_unchecked(&mut this.inner) };
            let poll_result = pinned.poll(cx);

            // 3. Before the scope guard drops, capture mutations back to snapshot.
            this.snapshot = crate::snapshot::snapshot();

            poll_result
            // 4. _guard drops here → scope popped, thread-local restored.
        });

        result
    }
}

/// Push a scope with snapshot values onto the thread-local stack.
/// Returns a ScopeGuard that pops on drop.
fn install_snapshot(snap: &ContextSnapshot) -> crate::scope::ScopeGuard {
    let guard = storage::enter_scope();
    // Restore the scope chain from the snapshot.
    if !snap.scope_chain.is_empty() {
        storage::set_remote_chain(snap.scope_chain.clone());
    }
    for (key, val) in snap.values.iter() {
        storage::set_value(key, val.clone_boxed());
    }
    guard
}

/// Capture the current **thread-local** context and wrap a future so it carries
/// that context through any async executor. This is the runtime-agnostic
/// alternative to [`with_context`](crate::with_context) (which requires Tokio).
///
/// Uses `force_thread_local` internally to snapshot from thread-local storage,
/// since `ContextFuture` operates entirely on thread-local state. If you have
/// context in a Tokio task-local and want to bridge to `ContextFuture`, call
/// [`snapshot()`](crate::snapshot) yourself and use [`ContextFuture::new`].
///
/// # Example
///
/// ```rust,ignore
/// use dcontext::{register, set_context, get_context, with_context_future};
///
/// register::<MyTraceId>("trace_id");
/// set_context("trace_id", MyTraceId("abc".into()));
///
/// // Works with any executor
/// let fut = with_context_future(async {
///     let tid: MyTraceId = get_context("trace_id");
///     assert_eq!(tid.0, "abc");
/// });
/// ```
pub fn with_context_future<F>(future: F) -> ContextFuture<F>
where
    F: Future,
{
    let snap = storage::force_thread_local(|| crate::snapshot::snapshot());
    ContextFuture::new(snap, future)
}
