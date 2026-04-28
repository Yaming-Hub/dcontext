use std::sync::{Arc, Mutex, Once};

use tracing_subscriber::prelude::*;

use crate::{DcontextLayer, SpanInfo, TracingField, SPAN_INFO_KEY};

static INIT: Once = Once::new();

fn init_registry() {
    INIT.call_once(|| {
        let mut builder = dcontext::RegistryBuilder::new();
        builder.register::<String>("outer");
        builder.register::<String>("inner");
        builder.register::<String>("level");
        builder.register::<String>("visit");
        builder.register::<String>("tenant");
        // Extract + enrich via TracingField metadata
        builder.register_with::<String>("request_id", |opts| {
            opts.with_metadata(
                TracingField::builder("request_id")
                    .extract_from_str(|s| Some(s.to_string()))
                    .enrich_display::<String>()
                    .build(),
            )
        });
        builder.register_with::<Counter>("count", |opts| {
            opts.with_metadata(
                TracingField::builder("count")
                    .extract_from_u64(|v| Some(Counter(v)))
                    .enrich_debug::<Counter>()
                    .build(),
            )
        });
        builder.register_with::<Flag>("enabled", |opts| {
            opts.with_metadata(
                TracingField::builder("enabled")
                    .extract_from_bool(|v| Some(Flag(v)))
                    .build(),
            )
        });
        builder.register::<SpanInfo>(SPAN_INFO_KEY);
        // Enrich-only fields (no extraction)
        builder.register_with::<String>("log_rid", |opts| {
            opts.with_metadata(
                TracingField::builder("rid")
                    .enrich_display::<String>()
                    .build(),
            )
        });
        builder.register_with::<Counter>("log_counter", |opts| {
            opts.with_metadata(
                TracingField::builder("cnt")
                    .enrich_debug::<Counter>()
                    .build(),
            )
        });
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
            let val: String = dcontext::get_context("outer");
            assert_eq!(val, "hello");

            dcontext::set_context("inner", "world".to_string());
            let inner: String = dcontext::get_context("inner");
            assert_eq!(inner, "world");
        }

        let inner: String = dcontext::get_context("inner");
        assert_eq!(inner, "", "inner should be empty after span exit");

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

        {
            let _entered = span.enter();
            dcontext::set_context("visit", "first".to_string());
        }

        let val: String = dcontext::get_context("visit");
        assert_eq!(val, "", "should be reverted after first exit");

        {
            let _entered = span.enter();
            let val: String = dcontext::get_context("visit");
            assert_eq!(val, "", "should be empty on re-enter");
        }
    });
}

// --- Level 2: Field extraction tests (via TracingField metadata) ---

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct Counter(u64);

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct Flag(bool);

#[test]
fn field_extraction_string() {
    with_layer(DcontextLayer::new(), || {
        {
            let _span = tracing::info_span!("handler", request_id = "abc-123").entered();
            let id: String = dcontext::get_context("request_id");
            assert_eq!(id, "abc-123");
        }

        let id: String = dcontext::get_context("request_id");
        assert_eq!(id, String::default());
    });
}

#[test]
fn field_extraction_u64() {
    with_layer(DcontextLayer::new(), || {
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
fn field_extraction_bool() {
    with_layer(DcontextLayer::new(), || {
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
fn field_extraction_missing_field() {
    with_layer(DcontextLayer::new(), || {
        {
            // Span without the mapped field — should not set anything
            let _span = tracing::info_span!("handler", other_field = "value").entered();
            let id: String = dcontext::get_context("request_id");
            assert_eq!(id, String::default());
        }
    });
}

#[test]
fn field_extraction_late_record() {
    with_layer(DcontextLayer::new(), || {
        let span = tracing::info_span!("handler", request_id = tracing::field::Empty);

        span.record("request_id", "late-value");

        {
            let _entered = span.enter();
            let id: String = dcontext::get_context("request_id");
            assert_eq!(id, "late-value");
        }
    });
}

#[test]
fn multiple_field_extractions() {
    with_layer(DcontextLayer::new(), || {
        {
            let _span =
                tracing::info_span!("handler", request_id = "abc", count = 10u64).entered();
            let id: String = dcontext::get_context("request_id");
            let c: Counter = dcontext::get_context("count");
            assert_eq!(id, "abc");
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

            let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
            assert_eq!(info.name, "outer");
        }
    });
}

// --- Combined features test ---

#[test]
fn all_features_combined() {
    let layer = DcontextLayer::builder()
        .include_span_info()
        .build();

    with_layer(layer, || {
        dcontext::set_context("tenant", "acme".to_string());

        {
            let _span = tracing::info_span!("process", request_id = "req-001").entered();

            let id: String = dcontext::get_context("request_id");
            assert_eq!(id, "req-001");

            let info: SpanInfo = dcontext::get_context(SPAN_INFO_KEY);
            assert_eq!(info.name, "process");

            let tenant: String = dcontext::get_context("tenant");
            assert_eq!(tenant, "acme");
        }

        let id: String = dcontext::get_context("request_id");
        assert_eq!(id, String::default());
    });
}

// --- Async tests ---

#[tokio::test]
async fn async_with_instrument() {
    use tracing::Instrument;

    init_registry();
    let layer = DcontextLayer::new();

    let subscriber = tracing_subscriber::registry().with(layer);
    let _guard = tracing::subscriber::set_default(subscriber);

    async fn inner_task() {
        let id: String = dcontext::force_thread_local(|| dcontext::get_context("request_id"));
        assert_eq!(id, "async-001");
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

        let info: SpanInfo =
            dcontext::force_thread_local(|| dcontext::get_context(SPAN_INFO_KEY));
        assert_eq!(info.name, "outer_span");
    }

    outer()
        .instrument(tracing::info_span!("outer_span"))
        .await;
}

// --- Scope chain tests ---

#[test]
fn scope_chain_from_span_names() {
    with_layer(DcontextLayer::new(), || {
        let _outer = tracing::info_span!("api_handler").entered();
        assert_eq!(
            dcontext::force_thread_local(dcontext::scope_chain),
            vec!["api_handler"]
        );

        let _inner = tracing::info_span!("db_query").entered();
        assert_eq!(
            dcontext::force_thread_local(dcontext::scope_chain),
            vec!["api_handler", "db_query"]
        );
    });
}

#[test]
fn field_extraction_string_directly() {
    // String extraction works directly via TracingField metadata
    with_layer(DcontextLayer::new(), || {
        {
            let _span = tracing::info_span!("handler", request_id = "direct-str").entered();
            let t: String = dcontext::force_thread_local(|| dcontext::get_context("request_id"));
            assert_eq!(t, "direct-str");
        }

        let t: String = dcontext::force_thread_local(|| dcontext::get_context("request_id"));
        assert_eq!(t, String::default());
    });
}

#[test]
fn scope_chain_reverts_on_exit() {
    with_layer(DcontextLayer::new(), || {
        let _outer = tracing::info_span!("root").entered();
        {
            let _inner = tracing::info_span!("child").entered();
            assert_eq!(
                dcontext::force_thread_local(dcontext::scope_chain),
                vec!["root", "child"]
            );
        }
        assert_eq!(
            dcontext::force_thread_local(dcontext::scope_chain),
            vec!["root"]
        );
    });
}

// --- Log enrichment tests ---

#[test]
fn collect_log_fields_returns_set_values() {
    init_registry();
    dcontext::force_thread_local(|| {
        let _g = dcontext::enter_scope();
        dcontext::set_context("log_rid", "req-123".to_string());
        dcontext::set_context("log_counter", Counter(42));

        let fields = crate::collect_log_fields();
        let map: std::collections::HashMap<&str, &str> =
            fields.iter().map(|(k, v)| (*k, v.as_str())).collect();

        assert_eq!(map.get("rid"), Some(&"req-123"));
        assert_eq!(map.get("cnt"), Some(&"Counter(42)"));
    });
}

#[test]
fn collect_log_fields_skips_unset_values() {
    init_registry();
    dcontext::force_thread_local(|| {
        let _g = dcontext::enter_scope();
        let fields = crate::collect_log_fields();
        let names: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
        assert!(!names.contains(&"rid"));
        assert!(!names.contains(&"cnt"));
    });
}

#[test]
fn tracing_field_display_formatting() {
    let tf = TracingField::builder("test")
        .enrich_display::<String>()
        .build();
    let val = "hello".to_string();
    let any_val: &dyn std::any::Any = &val;
    assert_eq!(tf.format(any_val), Some("hello".to_string()));
}

#[test]
fn tracing_field_debug_formatting() {
    let tf = TracingField::builder("test")
        .enrich_debug::<Counter>()
        .build();
    let val = Counter(7);
    let any_val: &dyn std::any::Any = &val;
    assert_eq!(tf.format(any_val), Some("Counter(7)".to_string()));
}

#[test]
fn tracing_field_custom_formatting() {
    let tf = TracingField::builder("cnt")
        .enrich_custom::<Counter>(|c| format!("count={}", c.0))
        .build();
    let val = Counter(99);
    let any_val: &dyn std::any::Any = &val;
    assert_eq!(tf.format(any_val), Some("count=99".to_string()));
}

#[test]
fn tracing_field_wrong_type_returns_none() {
    let tf = TracingField::builder("test")
        .enrich_display::<String>()
        .build();
    let val: u64 = 42;
    let any_val: &dyn std::any::Any = &val;
    assert_eq!(tf.format(any_val), None);
}

#[test]
fn tracing_field_no_enrich_returns_none() {
    // Extract-only field: format returns None
    let tf = TracingField::builder("test")
        .extract_from_str(|s| Some(s.to_string()))
        .build();
    let val = "hello".to_string();
    let any_val: &dyn std::any::Any = &val;
    assert_eq!(tf.format(any_val), None);
}

#[test]
fn with_context_fields_enriches_output() {
    use tracing_subscriber::fmt;

    init_registry();

    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_clone = Arc::clone(&buf);

    let writer = move || -> Box<dyn std::io::Write + Send> {
        Box::new(WriterCapture(Arc::clone(&buf_clone)))
    };

    let fmt_layer = fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_level(false)
        .with_target(false)
        .event_format(crate::WithContextFields::wrap(
            fmt::format().without_time().with_ansi(false).with_level(false).with_target(false),
        ));

    let subscriber = tracing_subscriber::registry()
        .with(DcontextLayer::new())
        .with(fmt_layer);

    let _guard = tracing::subscriber::set_default(subscriber);

    dcontext::force_thread_local(|| {
        let _scope = dcontext::enter_scope();
        dcontext::set_context("log_rid", "req-abc".to_string());

        tracing::info!("test event");
    });

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("rid=req-abc"),
        "expected rid=req-abc in output: {}",
        output
    );
    assert!(
        output.contains("test event"),
        "expected 'test event' in output: {}",
        output
    );
}

/// Helper writer that captures output into a shared buffer.
struct WriterCapture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for WriterCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
