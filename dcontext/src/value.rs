use std::any::Any;

use crate::error::ContextError;

/// Type-erased context value. Stored as `Arc<dyn ContextValue>` in the scope chain.
pub trait ContextValue: Any + Send + Sync {
    /// Downcast to &dyn Any.
    fn as_any(&self) -> &dyn Any;
    /// Serialize this value to bytes (bincode).
    /// Returns `LocalOnlyKey` for local-only (non-serializable) values.
    fn serialize_value(&self) -> Result<Vec<u8>, ContextError>;
    /// Whether this value is local-only (excluded from serialization).
    fn is_local(&self) -> bool;
}

impl<T> ContextValue for T
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn serialize_value(&self) -> Result<Vec<u8>, ContextError> {
        bincode::serialize(self).map_err(|e| ContextError::SerializationFailed(e.to_string()))
    }

    fn is_local(&self) -> bool {
        false
    }
}
