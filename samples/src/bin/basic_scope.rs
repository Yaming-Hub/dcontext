//! # Sample 1: Basic Scoped Context
//!
//! Demonstrates the core get/set/scope API.
//! Context changes in a child scope automatically revert when the scope exits.
//!
//! Usage: `cargo run --bin basic_scope`

use dcontext::{register, initialize, enter_scope, get_context, set_context, scope};
use serde::{Serialize, Deserialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct UserId(u64);

fn main() {
    // 1. Register context types at startup.
    register::<RequestId>("request_id");
    register::<UserId>("user_id");
    initialize();

    // 2. Set values in the root scope.
    set_context("request_id", RequestId("req-001".into()));
    set_context("user_id", UserId(42));
    println!("[root] request_id = {:?}", get_context::<RequestId>("request_id"));
    println!("[root] user_id    = {:?}", get_context::<UserId>("user_id"));

    // 3. Enter a child scope — changes here are isolated.
    {
        let _guard = enter_scope();
        set_context("request_id", RequestId("req-002-child".into()));
        println!("[child] request_id = {:?}", get_context::<RequestId>("request_id"));
        println!("[child] user_id    = {:?}", get_context::<UserId>("user_id")); // inherited
    }
    // Child scope exited — request_id reverted.
    println!("[root] request_id = {:?}", get_context::<RequestId>("request_id"));

    // 4. Closure-based scope (recommended API).
    scope(|| {
        set_context("user_id", UserId(99));
        println!("[scope] user_id = {:?}", get_context::<UserId>("user_id"));
    });
    println!("[root] user_id = {:?}", get_context::<UserId>("user_id")); // reverted to 42
}
