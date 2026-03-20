use std::sync::atomic::{AtomicUsize, Ordering};

/// 0 means no limit.
static MAX_CONTEXT_SIZE: AtomicUsize = AtomicUsize::new(0);

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
