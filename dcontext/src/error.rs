use thiserror::Error;

/// Errors returned by dcontext operations.
#[derive(Debug, Error)]
pub enum ContextError {
    #[error("context key '{0}' is not registered")]
    NotRegistered(String),

    #[error("context key '{0}' is already registered with a different type")]
    AlreadyRegistered(String),

    #[error("type mismatch for key '{0}': expected {1}, got {2}")]
    TypeMismatch(String, String, String),

    #[error("serialization failed: {0}")]
    SerializationFailed(String),

    #[error("deserialization failed: {0}")]
    DeserializationFailed(String),

    #[error("context size exceeds limit: {size} bytes > {limit} bytes")]
    ContextTooLarge { size: usize, limit: usize },

    #[error("key '{0}' is local-only and cannot be serialized")]
    LocalOnlyKey(String),

    #[error("no active scope: {0}")]
    NoActiveScope(String),

    #[error("registry is already initialized; registrations must happen before initialize()")]
    RegistryFrozen,
}
