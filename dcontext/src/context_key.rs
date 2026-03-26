use std::marker::PhantomData;

use crate::error::ContextError;
use crate::registry::RegistryBuilder;

/// A typed handle to a registered context entry.
///
/// Provides a type-safe, string-free API for get/set operations.
///
/// ```rust,ignore
/// static REQUEST_ID: ContextKey<RequestId> = ContextKey::new("request_id");
///
/// let mut builder = RegistryBuilder::new();
/// REQUEST_ID.register_on(&mut builder);
/// dcontext::initialize(builder);
///
/// REQUEST_ID.set(RequestId("req-123".into()));
/// let rid = REQUEST_ID.get();
/// ```
pub struct ContextKey<T: 'static> {
    key: &'static str,
    _marker: PhantomData<fn() -> T>,
}

impl<T> ContextKey<T>
where
    T: Clone + Default + Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    /// Create a new typed key handle. The key string is used for
    /// serialization and diagnostics only.
    pub const fn new(key: &'static str) -> Self {
        Self {
            key,
            _marker: PhantomData,
        }
    }

    /// Register this key on a builder. Panics on conflict.
    pub fn register_on(&self, builder: &mut RegistryBuilder) {
        builder.register::<T>(self.key);
    }

    /// Try to register this key on a builder.
    pub fn try_register_on(&self, builder: &mut RegistryBuilder) -> Result<(), ContextError> {
        builder.try_register::<T>(self.key)
    }

    /// Get the value. Returns `T::default()` if not set.
    /// Panics if the key is not registered.
    pub fn get(&self) -> T {
        crate::get_context::<T>(self.key)
    }

    /// Try to get the value.
    pub fn try_get(&self) -> Result<Option<T>, ContextError> {
        crate::try_get_context::<T>(self.key)
    }

    /// Set the value in the current scope.
    /// Panics if the key is not registered.
    pub fn set(&self, value: T) {
        crate::set_context::<T>(self.key, value);
    }

    /// Try to set the value.
    pub fn try_set(&self, value: T) -> Result<(), ContextError> {
        crate::try_set_context::<T>(self.key, value)
    }

    /// Get the string key name.
    pub fn key(&self) -> &'static str {
        self.key
    }
}
