//! # Sample 14: Custom Codec (JSON)
//!
//! Demonstrates `register_with` + `.codec()` for using a custom serialization
//! format instead of the default bincode. This is useful for cross-language
//! compatibility or when you need a self-describing format.
//!
//! Usage: `cargo run --bin custom_codec`

use dcontext::{
    register_with, register, set_context, get_context,
    scope, serialize_context, deserialize_context,
};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct AppConfig {
    feature_flags: Vec<String>,
    max_retries: u32,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

fn main() {
    println!("=== Custom Codec (JSON) ===\n");

    // Register AppConfig with JSON codec (self-describing, cross-language).
    register_with::<AppConfig>("app_config", |o| o.codec(
        |val| serde_json::to_vec(val).map_err(|e| e.to_string()),
        |bytes| serde_json::from_slice(bytes).map_err(|e| e.to_string()),
    ));

    // Register RequestId with default bincode (fast, compact).
    register::<RequestId>("request_id");

    // Set values.
    set_context("app_config", AppConfig {
        feature_flags: vec!["dark-mode".into(), "beta-api".into()],
        max_retries: 3,
    });
    set_context("request_id", RequestId("req-001".into()));

    println!("1. Mixed codecs in same context:");
    println!("   app_config (JSON):    {:?}", get_context::<AppConfig>("app_config"));
    println!("   request_id (bincode): {:?}", get_context::<RequestId>("request_id"));

    // Serialize — each key uses its own codec.
    let bytes = serialize_context().unwrap();
    println!("\n2. Serialized to {} bytes", bytes.len());

    // Deserialize — each key decoded with its registered codec.
    println!("\n3. Roundtrip:");
    scope(|| {
        let _guard = deserialize_context(&bytes).unwrap();
        println!("   app_config: {:?}", get_context::<AppConfig>("app_config"));
        println!("   request_id: {:?}", get_context::<RequestId>("request_id"));
    });

    println!("\nDone! Different keys can use different serialization formats.");
}
