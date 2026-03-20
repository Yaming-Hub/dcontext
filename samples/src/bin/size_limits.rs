//! # Sample 10: Context Size Limits
//!
//! Demonstrates configuring a maximum context size to prevent
//! unbounded context growth during serialization.
//!
//! Usage: `cargo run --bin size_limits`

use dcontext::{
    register, set_context,
    serialize_context, set_max_context_size, max_context_size,
    ContextError,
};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct Payload(String);

fn main() {
    register::<Payload>("payload");

    // Set a small payload — serialization works.
    set_context("payload", Payload("small".into()));
    let bytes = serialize_context().unwrap();
    println!("Small payload serialized: {} bytes", bytes.len());
    println!("Current limit: {} (0 = no limit)", max_context_size());

    // Set a size limit.
    println!("\nSetting max context size to 50 bytes...");
    set_max_context_size(50);
    println!("Current limit: {}", max_context_size());

    // Small payload still fits.
    match serialize_context() {
        Ok(bytes) => println!("Small payload OK: {} bytes", bytes.len()),
        Err(e) => println!("Error: {}", e),
    }

    // Set a large payload that exceeds the limit.
    let large = "x".repeat(200);
    set_context("payload", Payload(large));

    match serialize_context() {
        Ok(bytes) => println!("Large payload OK: {} bytes (unexpected!)", bytes.len()),
        Err(ContextError::ContextTooLarge { size, limit }) => {
            println!("\nContextTooLarge: {} bytes > {} byte limit", size, limit);
        }
        Err(e) => println!("Other error: {}", e),
    }

    // Disable the limit.
    println!("\nDisabling size limit...");
    set_max_context_size(0);
    match serialize_context() {
        Ok(bytes) => println!("Large payload OK after disabling limit: {} bytes", bytes.len()),
        Err(e) => println!("Error: {}", e),
    }
}
