//! # Sample 1: Basic Scoped Context
//!
//! Demonstrates the core thread-local get/set/scope API.
//! Context changes in a child scope automatically revert when the scope exits.
//!
//! Usage: `cargo run --bin basic_scope`

use dcontext::{initialize, sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct UserId(u64);

fn main() {
    // 1. Register context types at startup.
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    builder.register::<UserId>("user_id");
    initialize(builder);

    // 2. Set values in the root scope.
    sync_ctx::set_context("request_id", RequestId("req-001".into()));
    sync_ctx::set_context("user_id", UserId(42));
    println!(
        "[root] request_id = {:?}",
        sync_ctx::get_context::<RequestId>("request_id").unwrap()
    );
    println!(
        "[root] user_id    = {:?}",
        sync_ctx::get_context::<UserId>("user_id").unwrap()
    );

    // 3. Enter a child scope — changes here are isolated.
    {
        let _guard = sync_ctx::enter_scope();
        sync_ctx::set_context("request_id", RequestId("req-002-child".into()));
        println!(
            "[child] request_id = {:?}",
            sync_ctx::get_context::<RequestId>("request_id").unwrap()
        );
        println!(
            "[child] user_id    = {:?}",
            sync_ctx::get_context::<UserId>("user_id").unwrap()
        ); // inherited
    }
    // Child scope exited — request_id reverted.
    println!(
        "[root] request_id = {:?}",
        sync_ctx::get_context::<RequestId>("request_id").unwrap()
    );

    // 4. Scoped override with an RAII guard.
    {
        let _guard = sync_ctx::enter_scope();
        sync_ctx::set_context("user_id", UserId(99));
        println!(
            "[scope] user_id = {:?}",
            sync_ctx::get_context::<UserId>("user_id").unwrap()
        );
    }
    println!(
        "[root] user_id = {:?}",
        sync_ctx::get_context::<UserId>("user_id").unwrap()
    ); // reverted to 42
}
