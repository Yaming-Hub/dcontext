//! # Sample 10: Context Size Limits
//!
//! Demonstrates configuring a maximum context size to prevent
//! unbounded context growth during serialization.
//!
//! Usage: `cargo run --bin size_limits`

use dcontext::{
    initialize, max_context_size, set_max_context_size, sync_ctx, ContextError, RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct Payload(String);

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<Payload>("payload");
    initialize(builder);

    // Set a small payload — serialization works.
    sync_ctx::set_context("payload", Payload("small".into()));
    let bytes = sync_ctx::serialize_context().unwrap();
    println!("Small payload serialized: {} bytes", bytes.len());
    println!("Current limit: {} (0 = no limit)", max_context_size());

    // Set a size limit.
    println!("\nSetting max context size to 50 bytes...");
    set_max_context_size(50);
    println!("Current limit: {}", max_context_size());

    // Small payload still fits.
    match sync_ctx::serialize_context() {
        Ok(bytes) => println!("Small payload OK: {} bytes", bytes.len()),
        Err(e) => println!("Error: {}", e),
    }

    // Set a large payload that exceeds the limit.
    let large = "x".repeat(200);
    sync_ctx::set_context("payload", Payload(large));

    match sync_ctx::serialize_context() {
        Ok(bytes) => println!("Large payload OK: {} bytes (unexpected!)", bytes.len()),
        Err(ContextError::ContextTooLarge { size, limit }) => {
            println!("\nContextTooLarge: {} bytes > {} byte limit", size, limit);
        }
        Err(e) => println!("Other error: {}", e),
    }

    // Disable the limit.
    println!("\nDisabling size limit...");
    set_max_context_size(0);
    match sync_ctx::serialize_context() {
        Ok(bytes) => println!(
            "Large payload OK after disabling limit: {} bytes",
            bytes.len()
        ),
        Err(e) => println!("Error: {}", e),
    }
}
