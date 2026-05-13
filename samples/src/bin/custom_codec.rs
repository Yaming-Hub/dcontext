//! # Sample 14: Custom Codec (JSON)
//!
//! Demonstrates `register_with` + `.codec()` for using a custom serialization
//! format instead of the default bincode.
//!
//! Usage: `cargo run --bin custom_codec`

use dcontext::{
    attach_snapshot, capture, get_context_variable, initialize, set_context_variable,
    ContextSnapshot, RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct AppConfig {
    feature_flags: Vec<String>,
    max_retries: u32,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

fn main() {
    println!("=== Custom Codec (JSON) ===\n");

    let mut builder = RegistryBuilder::new();
    builder.register_with::<AppConfig>("app_config", |o| {
        o.codec(
            |val| serde_json::to_vec(val).map_err(|e| e.to_string()),
            |bytes| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
        )
    });
    builder.register::<RequestId>("request_id");
    initialize(builder);

    set_context_variable(
        "app_config",
        AppConfig {
            feature_flags: vec!["dark-mode".into(), "beta-api".into()],
            max_retries: 3,
        },
    );
    set_context_variable("request_id", RequestId("req-001".into()));

    println!("1. Mixed codecs in same context:");
    println!(
        "   app_config (JSON):    {:?}",
        get_context_variable::<AppConfig>("app_config").unwrap()
    );
    println!(
        "   request_id (bincode): {:?}",
        get_context_variable::<RequestId>("request_id").unwrap()
    );

    let bytes = capture().serialize().unwrap();
    println!("\n2. Serialized to {} bytes", bytes.len());

    println!("\n3. Roundtrip:");
    {
        let snap = ContextSnapshot::deserialize(&bytes).unwrap();
        let _guard = attach_snapshot(snap);
        println!(
            "   app_config: {:?}",
            get_context_variable::<AppConfig>("app_config").unwrap()
        );
        println!(
            "   request_id: {:?}",
            get_context_variable::<RequestId>("request_id").unwrap()
        );
    }

    println!("\nDone! Different keys can use different serialization formats.");
}
