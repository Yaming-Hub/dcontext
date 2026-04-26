use std::sync::Once;

use tracing_subscriber::prelude::*;

use crate::{DcontextLayer, FromFieldValue, SpanInfo, SPAN_INFO_KEY};

static INIT: Once = Once::new();

fn init_registry() {
    INIT.call_once(|| {
        let mut builder = dcontext::RegistryBuilder::new();
        builder.register::<String>("outer");
        builder.register::<String>("inner");
        builder.register::<String>("level");
        builder.register::<String>("visit");
        builder.register::<String>("tenant");
        builder.register::<RequestId>("request_id");
        builder.register::<Counter>("count");
        builder.register::<Flag>("enabled");
        builder.register::<SpanInfo>(SPAN_INFO_KEY);
        let _ = dcontext::try_initialize(builder);
    });
}

// Helper: set up a subscriber with DcontextLayer for testing
fn with_layer<F: FnOnce()>(layer: DcontextLayer<tracing_subscriber::Registry>, f: F) {
    init_registry();
    let subscriber = tracing_subscriber::registry().with(layer);
    let _guard = tracing::subscriber::set_default(subscriber);
    f();
}

// --- Level 1: Auto-scoping tests ---

#[test]
fn basic_scope_enter_exit() {
    with_layer(DcontextLayer::new(), || {
        dcontext::set_context("outer", "hello".to_string());

        {
            let _span = tracing::info_span!("my_span").entered();
            // Inside span: new scope inherits parent values
            let val: String = dcontext::get_context("outer");
            assert_eq!(val, "hello");

            // Set a value in the span scope
            dcontext::set_context("inner", "world".to_string());
            let inner: String = dcontext::get_context("inner");
            assert_eq!(inner, "world");
        }

        // After span exit: inner value should be reverted
        let inner: String = dcontext::get_context("inner");
        assert_eq!(inner, "", "inner should be empty after span exit");

        // Outer value still accessible
        let outer: String = dcontext::get_context("outer");
        assert_eq!(outer, "hello");
    });
}

#[test]
fn nested_spans() {
    with_layer(DcontextLayer::new(), || {
        dcontext::set_context("level", "root".to_string());

        {
            let _span1 = tracing::info_span!("span1").entered();
            dcontext::set_context("level", "span1".to_string());

            {
                let _span2 = tracing::info_span!("span2").entered();
                dcontext::set_context("level", "span2".to_string());

                let val: String = dcontext::get_context("level");
                assert_eq!(val, "span2");
            }

            let val: String = dcontext::get_context("level");
            assert_eq!(val, "span1");
        }

        let val: String = dcontext::get_context("level");
        assert_eq!(val, "root");
    });
}

#[test]
fn span_reenter() {
    with_layer(DcontextLayer::new(), || {
        let span = tracing::info_span!("reentrant");

        // First enter
        {
            let _entered = span.enter();
            dcontext::set_context("visit", "first".to_string());
        }

        // After first exit
        let val: String = dcontext::get_context("visit");
        assert_eq!(val, "", "should be reverted after first exit");

        // Second enter — fresh scope
        {
            let _entered = span.enter();
            let val: String = dcontext::get_context("visit");
            assert_eq!(val, "", "should be empty on re-enter");
        }
    });
}

// --- Level 2: Field mapping tests ---

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct RequestId(String);

impl FromFieldValue for RequestId {
    fn from_str_value(s: &str) -> Option<Self> {
        Some(RequestId(s.to_string()))
    }
}

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct Counter(u64);

impl FromFieldValue for Counter {
    fn from_u64_value(v: u64) -> Option<Self> {
        Some(Counter(v))
    }
}

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct Flag(bool);

impl FromFieldValue for Flag {
    fn from_bool_value(v: bool) -> Option<Self> {
        Some(Flag(v))
    }
}

#[test]
fn field_mapping_string() {
    let layer = DcontextLayer::builder()
        .map_field::<RequestId>("request_id")
        .build();

    with_layer(layer, || {
        {
            let _span = tracing::info_span!("handler", request_id = "abc-123").entered();
            let id: RequestId = dcontext::get_context("request_id");
            assert_eq!(id, RequestId("abc-123".to_string()));
        }

        // After span exit, the mapped value should be reverted
        let id: RequestId = dcontext::get_context("request_id");
        assert_eq!(id, RequestId::default());
    });
}

#[test]
fn field_mapping_u64() {
    let layer = DcontextLayer::builder()
        .map_field::<Counter>("count")
        .build();

    with_layer(layer, || {
        {
            let _span = tracing::info_span!("handler", count = 42u64).entered();
            let c: Counter = dcontext::get_context("count");
            assert_eq!(c, Counter(42));
        }

        let c: Counter = dcontext::get_context("count");
        assert_eq!(c, Counter::default());
    });
}

#[test]
fn field_mapping_bool() {
    let layer = DcontextLayer::builder()
        .map_field::<Flag>("enabled")
        .build();

    with_layer(layer, || {
        {
            let _span = tracing::info_span!("handler", enabled = true).entered();
            let f: Flag = dcontext::get_context("enabled");
            assert_eq!(f, Flag(true));
        }

        let f: Flag = dcontext::get_context("enabled");
        assert_eq!(f, Flag::default());
    });
}

#[test]
fn field_mapping_with_rename() {
    let layer = DcontextLayer::builder()
        .map_field_as::<RequestId>("req_id", "request_id")
        .build();

    with_layer(layer, || {
        {
            let _span = tracing::info_span!("handler", req_id = "xyz-789").entered();
            // The field "req_id" is mapped to context key "request_id"
            let id: RequestId = dcontext::get_context("request_id");
            assert_eq!(id, RequestId("xyz-789".to_string()));
        }
    });
}

#[test]
fn field_mapping_missing_field() {
    let layer = DcontextLayer::builder()
        .map_field::<RequestId>("request_id")
        .build();

    with_layer(layer, || {
        {
            // Span without the mapped field — should not set anything
            let _span = tracing::info_span!("handler", other_field = "value").entered();
            let id: RequestId = dcontext::get_context("request_id");
            assert_eq!(id, RequestId::default());
        }
    });
}

#[test]
fn multiple_field_mappings() {
    let layer = DcontextLayer::builder()
        .map_field::<RequestId>("request_id")
        .map_field::<Counter>("count")
        .build();

    with_layer(layer, || {
        {
            let _span =
                tracing::info_span!("handler", request_id = "abc", count = 10u64).entered();
            let id: RequestId = dcontext::get_context("request_id");
            let c: Counter = dcontext::get_context("count");
            assert_eq!(id, RequestId("abc".to_string()));
            assert_eq!(c, Counter(10));
        }
    });
}

// --- Level 3: Span info tests ---

#[test]
fn span_info_basic() {
    let layer = DcontextLayer::builder().include_span_info().build();

    with_layer(layer, || {
        {
            let _span = tracing::info_span!("my_operation").entered();
            let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
            assert_eq!(info.name, "my_operation");
            assert!(
                info.target.contains("tests"),
                "target should contain module name, got: {}",
                info.target
            );
            assert_eq!(info.level, "INFO");
        }

        // After exit, reverted
        let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
        assert_eq!(info.name, "");
    });
}

#[test]
fn span_info_different_levels() {
    let layer = DcontextLayer::builder().include_span_info().build();

    with_layer(layer, || {
        {
            let _span = tracing::debug_span!("debug_op").entered();
            let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
            assert_eq!(info.level, "DEBUG");
        }
        {
            let _span = tracing::warn_span!("warn_op").entered();
            let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
            assert_eq!(info.level, "WARN");
        }
    });
}

#[test]
fn span_info_nested_shows_innermost() {
    let layer = DcontextLayer::builder().include_span_info().build();

    with_layer(layer, || {
        {
            let _span1 = tracing::info_span!("outer").entered();
            let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
            assert_eq!(info.name, "outer");

            {
                let _span2 = tracing::info_span!("inner").entered();
                let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
                assert_eq!(info.name, "inner");
            }

            // Back to outer
            let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
            assert_eq!(info.name, "outer");
        }
    });
}

// --- Combined features test ---

#[test]
fn all_features_combined() {
    let layer = DcontextLayer::builder()
        .map_field::<RequestId>("request_id")
        .include_span_info()
        .build();

    with_layer(layer, || {
        dcontext::set_context("tenant", "acme".to_string());

        {
            let _span = tracing::info_span!("process", request_id = "req-001").entered();

            // Field mapping works
            let id: RequestId = dcontext::get_context("request_id");
            assert_eq!(id, RequestId("req-001".to_string()));

            // Span info works
            let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
            assert_eq!(info.name, "process");

            // Inherited context works
            let tenant: String = dcontext::get_context("tenant");
            assert_eq!(tenant, "acme");
        }

        // All reverted
        let id: RequestId = dcontext::get_context("request_id");
        assert_eq!(id, RequestId::default());
    });
}

// --- Async tests ---

#[tokio::test]
async fn async_with_instrument() {
    use tracing::Instrument;

    init_registry();
    let layer = DcontextLayer::builder()
        .map_field::<RequestId>("request_id")
        .build();

    let subscriber = tracing_subscriber::registry().with(layer);
    let _guard = tracing::subscriber::set_default(subscriber);

    async fn inner_task() {
        let id: RequestId = dcontext::force_thread_local(|| dcontext::get_context("request_id"));
        assert_eq!(id, RequestId("async-001".to_string()));
    }

    inner_task()
        .instrument(tracing::info_span!("async_handler", request_id = "async-001"))
        .await;
}

#[tokio::test]
async fn async_nested_instrument() {
    use tracing::Instrument;

    init_registry();
    let layer = DcontextLayer::builder().include_span_info().build();

    let subscriber = tracing_subscriber::registry().with(layer);
    let _guard = tracing::subscriber::set_default(subscriber);

    async fn outer() {
        let info: SpanInfo =
            dcontext::force_thread_local(|| dcontext::get_context(SPAN_INFO_KEY));
        assert_eq!(info.name, "outer_span");

        async fn inner() {
            let info: SpanInfo =
                dcontext::force_thread_local(|| dcontext::get_context(SPAN_INFO_KEY));
            assert_eq!(info.name, "inner_span");
        }

        inner()
            .instrument(tracing::info_span!("inner_span"))
            .await;

        // Back to outer after inner completes
        let info: SpanInfo =
            dcontext::force_thread_local(|| dcontext::get_context(SPAN_INFO_KEY));
        assert_eq!(info.name, "outer_span");
    }

    outer()
        .instrument(tracing::info_span!("outer_span"))
        .await;
}
