use std::sync::atomic::{AtomicUsize, Ordering};

/// 0 means no limit.
static MAX_CONTEXT_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Default scope chain length limit.
const DEFAULT_MAX_SCOPE_CHAIN_LEN: usize = 64;

/// 0 means no limit; non-zero caps the scope chain to that many entries.
static MAX_SCOPE_CHAIN_LEN: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_SCOPE_CHAIN_LEN);

/// Set the maximum serialized context size in bytes.
/// Serialization functions will return `ContextTooLarge` if exceeded.
/// Set to 0 to disable (default).
pub fn set_max_context_size(limit: usize) {
    MAX_CONTEXT_SIZE.store(limit, Ordering::Relaxed);
}

/// Get the current max context size limit. 0 means no limit.
pub fn max_context_size() -> usize {
    MAX_CONTEXT_SIZE.load(Ordering::Relaxed)
}

/// Check if a serialized size exceeds the configured limit.
/// Returns Ok(()) if within limit or no limit set.
pub(crate) fn check_size(size: usize) -> Result<(), crate::error::ContextError> {
    let limit = max_context_size();
    if limit > 0 && size > limit {
        Err(crate::error::ContextError::ContextTooLarge { size, limit })
    } else {
        Ok(())
    }
}

/// Set the maximum number of entries in the scope chain.
///
/// When the chain exceeds this limit, the oldest entries (from the remote
/// prefix) are dropped to keep the most recent scopes visible.
/// Set to 0 to disable the limit. Default is 64.
pub fn set_max_scope_chain_len(limit: usize) {
    MAX_SCOPE_CHAIN_LEN.store(limit, Ordering::Relaxed);
}

/// Get the current max scope chain length. 0 means no limit. Default is 64.
pub fn max_scope_chain_len() -> usize {
    MAX_SCOPE_CHAIN_LEN.load(Ordering::Relaxed)
}
