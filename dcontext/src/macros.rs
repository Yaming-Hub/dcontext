/// Register multiple context types at once.
///
/// ```rust,ignore
/// dcontext::register_contexts! {
///     "trace_context" => TraceContext,
///     "feature_flags" => FeatureFlags,
///     "auth_info"     => AuthInfo,
/// }
/// ```
#[macro_export]
macro_rules! register_contexts {
    ( $( $key:expr => $ty:ty ),* $(,)? ) => {
        $(
            $crate::register::<$ty>($key);
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
