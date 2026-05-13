//! # Sample 7: ContextKey<T> — Typed Key API
//!
//! Demonstrates the `ContextKey<T>` typed wrapper.
//!
//! Usage: `cargo run --bin typed_keys`

use dcontext::{push_scope, ContextKey, RegistryBuilder};
use serde::{Deserialize, Serialize};

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
    let mut builder = RegistryBuilder::new();
    REQUEST_ID.register_on(&mut builder);
    USER_INFO.register_on(&mut builder);
    FEATURE_FLAGS.register_on(&mut builder);

    let another: ContextKey<RequestId> = ContextKey::new("another_key");
    another.register_on(&mut builder);

    dcontext::initialize(builder);

    REQUEST_ID.set(RequestId("req-typed-001".into()));
    USER_INFO.set(UserInfo {
        id: 42,
        name: "Alice".into(),
    });
    FEATURE_FLAGS.set(Flags { dark_mode: true });

    println!("request_id = {:?}", REQUEST_ID.get().unwrap());
    println!("user_info  = {:?}", USER_INFO.get().unwrap());
    println!("dark_mode  = {}", FEATURE_FLAGS.get().unwrap().dark_mode);

    {
        let _guard = push_scope("child-request");
        REQUEST_ID.set(RequestId("req-typed-002-child".into()));
        println!(
            "\n[child scope] request_id = {:?}",
            REQUEST_ID.get().unwrap()
        );
        println!("[child scope] user_info  = {:?}", USER_INFO.get().unwrap());
    }

    println!(
        "\n[after scope] request_id = {:?}",
        REQUEST_ID.get().unwrap()
    );

    match another.get() {
        None => println!("\n'another_key' is registered but not set"),
        Some(v) => println!("\n'another_key' = {:?}", v),
    }
}
