/// Register multiple context types on a builder at once.
///
/// ```rust,ignore
/// let mut builder = dcontext::RegistryBuilder::new();
/// dcontext::register_contexts!(builder, {
///     "trace_context" => TraceContext,
///     "feature_flags" => FeatureFlags,
///     "auth_info"     => AuthInfo,
/// });
/// dcontext::initialize(builder);
/// ```
#[macro_export]
macro_rules! register_contexts {
    ( $builder:expr, { $( $key:expr => $ty:ty ),* $(,)? } ) => {
        $(
            $builder.register::<$ty>($key);
        )*
    };
}
