//! # Sample 10: Context Size Limits
//!
//! Demonstrates configuring a maximum context size during snapshot serialization.
//!
//! Usage: `cargo run --bin size_limits`

use dcontext::{
    capture, initialize, max_context_size, set_context_variable, set_max_context_size,
    ContextError, RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct Payload(String);

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<Payload>("payload");
    initialize(builder);

    set_context_variable("payload", Payload("small".into()));
    let bytes = capture().serialize().unwrap();
    println!("Small payload serialized: {} bytes", bytes.len());
    println!("Current limit: {} (0 = no limit)", max_context_size());

    println!("\nSetting max context size to 50 bytes...");
    set_max_context_size(50);
    println!("Current limit: {}", max_context_size());

    match capture().serialize() {
        Ok(bytes) => println!("Small payload OK: {} bytes", bytes.len()),
        Err(e) => println!("Error: {}", e),
    }

    set_context_variable("payload", Payload("x".repeat(200)));

    match capture().serialize() {
        Ok(bytes) => println!("Large payload OK: {} bytes (unexpected!)", bytes.len()),
        Err(ContextError::ContextTooLarge { size, limit }) => {
            println!("\nContextTooLarge: {} bytes > {} byte limit", size, limit);
        }
        Err(e) => println!("Other error: {}", e),
    }

    println!("\nDisabling size limit...");
    set_max_context_size(0);
    match capture().serialize() {
        Ok(bytes) => println!(
            "Large payload OK after disabling limit: {} bytes",
            bytes.len()
        ),
        Err(e) => println!("Error: {}", e),
    }
}
