//! # Sample 7: ContextKey<T> — Typed Key API
//!
//! Demonstrates the `ContextKey<T>` typed wrapper which provides
//! compile-time type safety without string keys at call sites.
//!
//! Usage: `cargo run --bin typed_keys`

use dcontext::{ContextKey, RegistryBuilder};
use serde::{Serialize, Deserialize};

// Define typed keys as statics — the string is only for serialization/diagnostics.
static REQUEST_ID: ContextKey<RequestId> = ContextKey::new("request_id");
static USER_INFO: ContextKey<UserInfo> = ContextKey::new("user_info");
static FEATURE_FLAGS: ContextKey<Flags> = ContextKey::new("feature_flags");

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct UserInfo {
    id: u64,
    name: String,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct Flags {
    dark_mode: bool,
}

fn main() {
    // Register all keys (type is inferred from the ContextKey).
    let mut builder = RegistryBuilder::new();
    REQUEST_ID.register_on(&mut builder);
    USER_INFO.register_on(&mut builder);
    FEATURE_FLAGS.register_on(&mut builder);

    // Register additional keys before freezing.
    let another: ContextKey<RequestId> = ContextKey::new("another_key");
    another.register_on(&mut builder);

    dcontext::initialize(builder);

    // Set values — no turbofish, no string key at call site.
    REQUEST_ID.set(RequestId("req-typed-001".into()));
    USER_INFO.set(UserInfo { id: 42, name: "Alice".into() });
    FEATURE_FLAGS.set(Flags { dark_mode: true });

    // Get values — fully type-safe.
    println!("request_id = {:?}", REQUEST_ID.get());
    println!("user_info  = {:?}", USER_INFO.get());
    println!("dark_mode  = {}", FEATURE_FLAGS.get().dark_mode);

    // Scoped override.
    dcontext::scope(|| {
        REQUEST_ID.set(RequestId("req-typed-002-child".into()));
        println!("\n[child scope] request_id = {:?}", REQUEST_ID.get());
        println!("[child scope] user_info  = {:?}", USER_INFO.get()); // inherited
    });

    println!("\n[after scope] request_id = {:?}", REQUEST_ID.get()); // reverted

    // try_get returns Ok(None) for registered-but-unset keys.
    match another.try_get() {
        Ok(None) => println!("\n'another_key' is registered but not set"),
        Ok(Some(v)) => println!("\n'another_key' = {:?}", v),
        Err(e) => println!("\nerror: {}", e),
    }
}
