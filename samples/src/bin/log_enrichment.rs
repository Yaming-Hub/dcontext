//! # Log Enrichment: Automatic Context in Log Output
//!
//! This sample demonstrates how to use `dcontext-tracing`'s log enrichment
//! feature to automatically inject context values into every log event.
//!
//! Key concepts:
//! 1. **TracingField metadata** — register context keys with extract/enrich behavior
//! 2. **WithContextFields** — wraps any formatter to prepend context fields
//! 3. **collect_log_fields()** — manual collection for custom formatters
//! 4. **Extensible metadata** — multiple crates can annotate the same key
//!
//! Run with: `cargo run --bin log_enrichment`

use dcontext_tracing::{DcontextLayer, TracingField, WithContextFields};
use serde::{Deserialize, Serialize};
use tracing_subscriber::prelude::*;

// ── Context types ──────────────────────────────────────────────

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct TenantId(String);

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RetryCount(u32);

// ── Setup ──────────────────────────────────────────────────────

fn init() -> tracing::subscriber::DefaultGuard {
    let mut builder = dcontext::RegistryBuilder::new();

    // Register with TracingField metadata — both extract and enrich.
    // The field name in logs can differ from the context key name.
    builder.register_with::<RequestId>("request_id", |opts| {
        opts.cached().with_metadata(
            TracingField::builder("rid")
                .extract_from_str(|s| Some(RequestId(s.to_string())))
                .enrich_display::<RequestId>()
                .build(),
        )
    });

    builder.register_with::<TenantId>("tenant_id", |opts| {
        opts.cached().with_metadata(
            TracingField::builder("tenant")
                .extract_from_str(|s| Some(TenantId(s.to_string())))
                .enrich_display::<TenantId>()
                .build(),
        )
    });

    // Debug formatter — uses {:?} output
    builder.register_with::<RetryCount>("retry_count", |opts| {
        opts.with_metadata(
            TracingField::builder("retry")
                .enrich_debug::<RetryCount>()
                .build(),
        )
    });

    // Custom formatter — any closure
    builder.register_with::<String>("environment", |opts| {
        opts.with_metadata(
            TracingField::builder("env")
                .enrich_custom::<String>(|s| s.to_uppercase())
                .build(),
        )
    });

    // This key has NO TracingField metadata — it will NOT appear in logs
    builder.register::<String>("internal_buffer");

    dcontext::initialize(builder);

    // Set up tracing with WithContextFields wrapper
    let layer = DcontextLayer::new();
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .event_format(WithContextFields::wrap(
            tracing_subscriber::fmt::format().with_target(false),
        ));

    tracing_subscriber::registry()
        .with(layer)
        .with(fmt_layer)
        .set_default()
}

// ── Demo functions ─────────────────────────────────────────────

fn demo_basic_enrichment() {
    println!("\n=== Basic Log Enrichment ===\n");

    let _scope = dcontext::enter_scope();

    // Set context values — only those with TracingField enrich metadata appear in logs
    dcontext::set_context("request_id", RequestId("req-abc-123".into()));
    dcontext::set_context("tenant_id", TenantId("acme-corp".into()));
    dcontext::set_context("internal_buffer", "secret-stuff".to_string());

    // These log lines will be prefixed with: rid=req-abc-123 tenant=acme-corp
    tracing::info!("handling incoming request");
    tracing::info!(items = 42, "query complete");
}

fn demo_nested_scopes() {
    println!("\n=== Nested Scopes ===\n");

    let _outer = dcontext::enter_scope();
    dcontext::set_context("request_id", RequestId("req-outer".into()));
    dcontext::set_context("environment", "production".to_string());

    tracing::info!("outer scope");

    {
        let _inner = dcontext::enter_scope();
        dcontext::set_context("request_id", RequestId("req-inner".into()));
        dcontext::set_context("retry_count", RetryCount(3));

        // Shows: rid=req-inner env=PRODUCTION retry=RetryCount(3)
        tracing::warn!("retrying operation");
    }

    // After inner scope exits, request_id reverts to "req-outer"
    // and retry_count is no longer set
    tracing::info!("back to outer scope");
}

fn demo_span_extraction() {
    println!("\n=== Span Field Extraction ===\n");

    // TracingField with extract_from_str auto-populates context from span fields
    {
        let _span = tracing::info_span!("handle_request", request_id = "req-from-span").entered();
        let rid: RequestId = dcontext::get_context("request_id");
        println!("  Extracted from span: request_id = {:?}", rid.0);

        // The enriched log also shows the extracted value
        tracing::info!("processing with extracted context");
    }
}

fn demo_collect_log_fields() {
    println!("\n=== Manual Field Collection ===\n");

    let _scope = dcontext::enter_scope();
    dcontext::set_context("request_id", RequestId("req-manual".into()));
    dcontext::set_context("tenant_id", TenantId("manual-tenant".into()));

    // collect_log_fields() returns (name, formatted_value) pairs
    let fields = dcontext_tracing::collect_log_fields();
    print!("  Collected fields: ");
    for (name, value) in &fields {
        print!("{}={} ", name, value);
    }
    println!();
}

fn demo_metadata_queries() {
    println!("\n=== Metadata Queries ===\n");

    // Query which keys have TracingField metadata
    let field_names: Vec<(&str, &str)> =
        dcontext::keys_with_metadata::<TracingField, _>(|key, tf| (key, tf.log_name()));

    println!("  Keys with TracingField metadata:");
    for (key, field_name) in &field_names {
        println!("    context key: {:?} → log field: {:?}", key, field_name);
    }

    // Query a specific key's metadata
    if let Some(name) = dcontext::with_metadata::<TracingField, _>("request_id", |tf| tf.log_name()) {
        println!("  request_id log field name: {:?}", name);
    }
}

fn demo_span_recording() {
    println!("\n=== Auto-Record into Span Fields ===\n");

    let _scope = dcontext::enter_scope();
    dcontext::set_context("request_id", RequestId("req-auto-record".into()));
    dcontext::set_context("tenant_id", TenantId("acme-auto".into()));

    // Span declares fields as Empty — DcontextLayer auto-fills them on enter.
    // This makes the values visible to ALL subscribers (OTel, fmt, custom layers).
    {
        let _span = tracing::info_span!(
            "process_order",
            rid = tracing::field::Empty,
            tenant = tracing::field::Empty,
        )
        .entered();

        // The span now has rid=req-auto-record and tenant=acme-auto recorded.
        // These appear in the span's fields in log output:
        tracing::info!("order processed with auto-recorded fields");
    }

    // Spans without the field are silently skipped — no error:
    {
        let _span = tracing::info_span!("simple_op").entered();
        tracing::info!("no fields to record, no problem");
    }
}

// ── Main ───────────────────────────────────────────────────────

fn main() {
    let _guard = init();

    println!("dcontext-tracing Sample: Log Enrichment");
    println!("========================================");

    demo_basic_enrichment();
    demo_nested_scopes();
    demo_span_extraction();
    demo_span_recording();
    demo_collect_log_fields();
    demo_metadata_queries();

    println!("\nDone!");
}
