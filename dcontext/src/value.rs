use std::any::Any;

use crate::error::ContextError;

/// Type-erased context value. Stored as trait objects in the scope stack.
pub(crate) trait ContextValue: Any + Send + Sync {
    /// Clone into a new boxed trait object.
    fn clone_boxed(&self) -> Box<dyn ContextValue>;
    /// Downcast to &dyn Any.
    fn as_any(&self) -> &dyn Any;
    /// Serialize this value to bytes (bincode).
    fn serialize_value(&self) -> Result<Vec<u8>, ContextError>;
    /// Type name for diagnostics.
    fn type_name(&self) -> &'static str;
}

impl<T> ContextValue for T
where
    T: Clone + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    fn clone_boxed(&self) -> Box<dyn ContextValue> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn serialize_value(&self) -> Result<Vec<u8>, ContextError> {
        bincode::serialize(self).map_err(|e| ContextError::SerializationFailed(e.to_string()))
    }

    fn type_name(&self) -> &'static str {
        std::any::type_name::<T>()
    }
}
