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

fn with_layer<F: FnOnce()>(layer: DcontextLayer<tracing_subscriber::Registry>, f: F) {
    init_registry();
    let subscriber = tracing_subscriber::registry().with(layer);
    let _guard = tracing::subscriber::set_default(subscriber);
    f();
}

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct Counter(u64);

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct Flag(bool);
// --- Field extraction tests ---

#[test]
fn field_extraction_string() {
    with_layer(DcontextLayer::new(), || {
        let _scope = dcontext::sync_ctx::enter_scope();
        let _span = tracing::info_span!("handler", request_id = "abc-123").entered();
        let id: String = dcontext::sync_ctx::get_context("request_id").unwrap_or_default();
        assert_eq!(id, "abc-123");
    });
}

#[test]
fn field_extraction_u64() {
    with_layer(DcontextLayer::new(), || {
        let _scope = dcontext::sync_ctx::enter_scope();
        let _span = tracing::info_span!("handler", count = 42u64).entered();
        let c: Counter = dcontext::sync_ctx::get_context("count").unwrap_or_default();
        assert_eq!(c, Counter(42));
    });
}

#[test]
fn field_extraction_bool() {
    with_layer(DcontextLayer::new(), || {
        let _scope = dcontext::sync_ctx::enter_scope();
        let _span = tracing::info_span!("handler", enabled = true).entered();
        let f: Flag = dcontext::sync_ctx::get_context("enabled").unwrap_or_default();
        assert_eq!(f, Flag(true));
    });
}

#[test]
fn field_extraction_missing_field() {
    with_layer(DcontextLayer::new(), || {
        let _scope = dcontext::sync_ctx::enter_scope();
        let _span = tracing::info_span!("handler", other_field = "value").entered();
        let id: String = dcontext::sync_ctx::get_context("request_id").unwrap_or_default();
        assert_eq!(id, String::default());
    });
}

#[test]
fn field_extraction_late_record() {
    with_layer(DcontextLayer::new(), || {
        let _scope = dcontext::sync_ctx::enter_scope();
        let span = tracing::info_span!("handler", request_id = tracing::field::Empty);
        span.record("request_id", "late-value");
        let _entered = span.enter();
        let id: String = dcontext::sync_ctx::get_context("request_id").unwrap_or_default();
        assert_eq!(id, "late-value");
    });
}

#[test]
fn multiple_field_extractions() {
    with_layer(DcontextLayer::new(), || {
        let _scope = dcontext::sync_ctx::enter_scope();
        let _span = tracing::info_span!("handler", request_id = "abc", count = 10u64).entered();
        let id: String = dcontext::sync_ctx::get_context("request_id").unwrap_or_default();
        let c: Counter = dcontext::sync_ctx::get_context("count").unwrap_or_default();
        assert_eq!(id, "abc");
        assert_eq!(c, Counter(10));
    });
}

// --- Span info tests ---

#[test]
fn span_info_basic() {
    let layer = DcontextLayer::builder().include_span_info().build();
    with_layer(layer, || {
        let _scope = dcontext::sync_ctx::enter_scope();
        let _span = tracing::info_span!("my_operation").entered();
        let info: SpanInfo = dcontext::sync_ctx::get_context(SPAN_INFO_KEY).unwrap_or_default();
        assert_eq!(info.name, "my_operation");
        assert!(info.target.contains("tests"), "target should contain module name, got: {}", info.target);
        assert_eq!(info.level, "INFO");
    });
}

#[test]
fn span_info_different_levels() {
    let layer = DcontextLayer::builder().include_span_info().build();
    with_layer(layer, || {
        let _scope = dcontext::sync_ctx::enter_scope();
        {
            let _span = tracing::debug_span!("debug_op").entered();
            let info: SpanInfo = dcontext::sync_ctx::get_context(SPAN_INFO_KEY).unwrap_or_default();
            assert_eq!(info.level, "DEBUG");
        }
        {
            let _span = tracing::warn_span!("warn_op").entered();
            let info: SpanInfo = dcontext::sync_ctx::get_context(SPAN_INFO_KEY).unwrap_or_default();
            assert_eq!(info.level, "WARN");
        }
    });
}

// --- Combined features test ---

#[test]
fn all_features_combined() {
    let layer = DcontextLayer::builder().include_span_info().build();
    with_layer(layer, || {
        let _scope = dcontext::sync_ctx::enter_scope();
        dcontext::sync_ctx::set_context("tenant", "acme".to_string());
        let _span = tracing::info_span!("process", request_id = "req-001").entered();

        let id: String = dcontext::sync_ctx::get_context("request_id").unwrap_or_default();
        assert_eq!(id, "req-001");
        let info: SpanInfo = dcontext::sync_ctx::get_context(SPAN_INFO_KEY).unwrap_or_default();
        assert_eq!(info.name, "process");
        let tenant: String = dcontext::sync_ctx::get_context("tenant").unwrap_or_default();
        assert_eq!(tenant, "acme");
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
        let id: String = dcontext::sync_ctx::get_context("request_id").unwrap_or_default();
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
        let info: SpanInfo = dcontext::sync_ctx::get_context(SPAN_INFO_KEY).unwrap_or_default();
        assert_eq!(info.name, "outer_span");

        async fn inner() {
            let info: SpanInfo = dcontext::sync_ctx::get_context(SPAN_INFO_KEY).unwrap_or_default();
            assert_eq!(info.name, "inner_span");
        }
        inner().instrument(tracing::info_span!("inner_span")).await;
    }

    outer().instrument(tracing::info_span!("outer_span")).await;
}

// --- Log enrichment tests ---

#[test]
fn collect_log_fields_returns_set_values() {
    init_registry();
    let _g = dcontext::sync_ctx::enter_scope();
    dcontext::sync_ctx::set_context("log_rid", "req-123".to_string());
    dcontext::sync_ctx::set_context("log_counter", Counter(42));
    let fields = crate::collect_log_fields();
    let map: std::collections::HashMap<&str, &str> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(map.get("rid"), Some(&"req-123"));
    assert_eq!(map.get("cnt"), Some(&"Counter(42)"));
}

#[test]
fn collect_log_fields_skips_unset_values() {
    init_registry();
    let _g = dcontext::sync_ctx::enter_scope();
    let fields = crate::collect_log_fields();
    let names: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
    assert!(!names.contains(&"rid"));
    assert!(!names.contains(&"cnt"));
}

#[test]
fn tracing_field_display_formatting() {
    let tf = TracingField::builder("test").enrich_display::<String>().build();
    let val = "hello".to_string();
    assert_eq!(tf.format(&val as &dyn std::any::Any), Some("hello".to_string()));
}

#[test]
fn tracing_field_debug_formatting() {
    let tf = TracingField::builder("test").enrich_debug::<Counter>().build();
    let val = Counter(7);
    assert_eq!(tf.format(&val as &dyn std::any::Any), Some("Counter(7)".to_string()));
}

#[test]
fn tracing_field_custom_formatting() {
    let tf = TracingField::builder("cnt").enrich_custom::<Counter>(|c| format!("count={}", c.0)).build();
    let val = Counter(99);
    assert_eq!(tf.format(&val as &dyn std::any::Any), Some("count=99".to_string()));
}

#[test]
fn tracing_field_wrong_type_returns_none() {
    let tf = TracingField::builder("test").enrich_display::<String>().build();
    let val: u64 = 42;
    assert_eq!(tf.format(&val as &dyn std::any::Any), None);
}

#[test]
fn tracing_field_no_enrich_returns_none() {
    let tf = TracingField::builder("test").extract_from_str(|s| Some(s.to_string())).build();
    let val = "hello".to_string();
    assert_eq!(tf.format(&val as &dyn std::any::Any), None);
}
struct WriterCapture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for WriterCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

#[test]
fn with_context_fields_enriches_output() {
    use tracing_subscriber::fmt;
    init_registry();
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_clone = Arc::clone(&buf);
    let writer = move || -> Box<dyn std::io::Write + Send> { Box::new(WriterCapture(Arc::clone(&buf_clone))) };
    let fmt_layer = fmt::layer()
        .with_writer(writer).with_ansi(false).with_level(false).with_target(false)
        .event_format(crate::WithContextFields::wrap(
            fmt::format().without_time().with_ansi(false).with_level(false).with_target(false),
        ));
    let subscriber = tracing_subscriber::registry().with(DcontextLayer::new()).with(fmt_layer);
    let _guard = tracing::subscriber::set_default(subscriber);
    {
        let _scope = dcontext::sync_ctx::enter_scope();
        dcontext::sync_ctx::set_context("log_rid", "req-abc".to_string());
        tracing::info!("test event");
    };
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(output.contains("rid=req-abc"), "expected rid=req-abc in output: {}", output);
    assert!(output.contains("test event"), "expected 'test event' in output: {}", output);
}

// --- Span recording tests ---

#[test]
fn span_record_auto_fills_empty_fields() {
    init_registry();
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_clone = Arc::clone(&buf);
    let writer = move || -> Box<dyn std::io::Write + Send> { Box::new(WriterCapture(Arc::clone(&buf_clone))) };
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer).with_ansi(false).without_time().with_target(false);
    let subscriber = tracing_subscriber::registry().with(DcontextLayer::new()).with(fmt_layer);
    let _guard = tracing::subscriber::set_default(subscriber);
    {
        let _scope = dcontext::sync_ctx::enter_scope();
        dcontext::sync_ctx::set_context("request_id", "req-auto-recorded".to_string());
        let _span = tracing::info_span!("handler", request_id = tracing::field::Empty).entered();
        tracing::info!("inside span");
    };
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("request_id") && output.contains("req-auto-recorded"),
        "expected request_id=req-auto-recorded in output: {}", output
    );
}

#[test]
fn span_record_skips_undeclared_fields() {
    init_registry();
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_clone = Arc::clone(&buf);
    let writer = move || -> Box<dyn std::io::Write + Send> { Box::new(WriterCapture(Arc::clone(&buf_clone))) };
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer).with_ansi(false).without_time().with_target(false);
    let subscriber = tracing_subscriber::registry().with(DcontextLayer::new()).with(fmt_layer);
    let _guard = tracing::subscriber::set_default(subscriber);
    {
        let _scope = dcontext::sync_ctx::enter_scope();
        dcontext::sync_ctx::set_context("request_id", "req-skip".to_string());
        let _span = tracing::info_span!("simple_op").entered();
        tracing::info!("no crash");
    };
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(output.contains("no crash"));
    assert!(!output.contains("req-skip"), "should not contain req-skip when field is undeclared: {}", output);
}

#[test]
fn enrich_display_shorthand_enables_both_log_and_span() {
    let tf = TracingField::builder("test").enrich_display::<String>().build();
    assert!(tf.has_log_enrich());
    assert!(tf.has_span_record());
    assert!(tf.has_enrich());
}

#[test]
fn enrich_log_only_does_not_enable_span() {
    let tf = TracingField::builder("test").enrich_log_display::<String>().build();
    assert!(tf.has_log_enrich());
    assert!(!tf.has_span_record());
    assert!(tf.has_enrich());
}

#[test]
fn enrich_span_only_does_not_enable_log() {
    let tf = TracingField::builder("test").enrich_span_display::<String>().build();
    assert!(!tf.has_log_enrich());
    assert!(tf.has_span_record());
    assert!(tf.has_enrich());
    let val = "hello".to_string();
    assert_eq!(tf.format(&val as &dyn std::any::Any), None);
    assert_eq!(tf.format_for_span(&val as &dyn std::any::Any), Some("hello".to_string()));
}

#[test]
fn record_as_overrides_field_name() {
    let tf = TracingField::builder("log_name").record_as("span_field_name").enrich_span_display::<String>().build();
    assert_eq!(tf.log_name(), "log_name");
    assert_eq!(tf.record_field(), "span_field_name");
}

#[test]
fn record_field_defaults_to_log_name() {
    let tf = TracingField::builder("my_field").enrich_span_display::<String>().build();
    assert_eq!(tf.record_field(), "my_field");
}

#[test]
fn span_record_does_not_overwrite_user_set_fields() {
    init_registry();
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_clone = Arc::clone(&buf);
    let writer = move || -> Box<dyn std::io::Write + Send> { Box::new(WriterCapture(Arc::clone(&buf_clone))) };
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer).with_ansi(false).without_time().with_target(false);
    let subscriber = tracing_subscriber::registry().with(DcontextLayer::new()).with(fmt_layer);
    let _guard = tracing::subscriber::set_default(subscriber);
    {
        let _scope = dcontext::sync_ctx::enter_scope();
        dcontext::sync_ctx::set_context("request_id", "from-context".to_string());
        let _span = tracing::info_span!("user_op", request_id = "explicit-value").entered();
        tracing::info!("check");
    };
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(output.contains("explicit-value"), "user-set value should be preserved: {}", output);
    assert!(!output.contains("from-context"), "context value should NOT overwrite user-set field: {}", output);
}

#[test]
fn self_recording_does_not_poison_extraction() {
    init_registry();
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_clone = Arc::clone(&buf);
    let writer = move || -> Box<dyn std::io::Write + Send> { Box::new(WriterCapture(Arc::clone(&buf_clone))) };
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer).with_ansi(false).without_time().with_target(false);
    let subscriber = tracing_subscriber::registry().with(DcontextLayer::new()).with(fmt_layer);
    let _guard = tracing::subscriber::set_default(subscriber);
    {
        let _scope = dcontext::sync_ctx::enter_scope();
        dcontext::sync_ctx::set_context("request_id", "original".to_string());
        let span = tracing::info_span!("poison_op", request_id = tracing::field::Empty);
        let _enter = span.enter();
        let val = dcontext::sync_ctx::get_context::<String>("request_id");
        assert_eq!(val, Some("original".to_string()), "context value must not be poisoned by self-recording");
        tracing::info!("verify");
    };
    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(output.contains("verify"));
}

// --- No-scoping behavior tests ---

#[test]
fn layer_does_not_create_scopes() {
    with_layer(DcontextLayer::new(), || {
        let _scope = dcontext::sync_ctx::enter_scope();
        let depth_before = dcontext::sync_ctx::current_depth();
        {
            let _span = tracing::info_span!("handler").entered();
            let depth_during = dcontext::sync_ctx::current_depth();
            assert_eq!(depth_before, depth_during, "layer should not push scopes");
        }
        let depth_after = dcontext::sync_ctx::current_depth();
        assert_eq!(depth_before, depth_after, "layer should not pop scopes");
    });
}

#[test]
fn scope_chain_not_affected_by_spans() {
    with_layer(DcontextLayer::new(), || {
        let _scope = dcontext::sync_ctx::enter_named_scope("app");
        let _span = tracing::info_span!("handler").entered();
        let chain = dcontext::sync_ctx::scope_chain();
        assert_eq!(chain, vec!["app"], "span names should not appear in scope chain");
    });
}