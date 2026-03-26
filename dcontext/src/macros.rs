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

/// Enter a scope, set values, execute a block, and auto-revert.
///
/// ```rust,ignore
/// dcontext::with_scope! {
///     "trace_id" => TraceId::new(),
///     "flags" => Flags { debug: true },
///     => {
///         do_work();
///     }
/// }
/// ```
#[macro_export]
macro_rules! with_scope {
    ( $( $key:expr => $val:expr ),+ $(,)? => $body:block ) => {
        $crate::scope(|| {
            $(
                $crate::set_context($key, $val);
            )*
            $body
        })
    };
}
