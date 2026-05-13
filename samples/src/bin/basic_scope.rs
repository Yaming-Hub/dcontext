//! # Sample 1: Basic Scoped Context
//!
//! Demonstrates the unified get/set/scope API.
//! Context changes in a child scope automatically revert when the scope exits.
//!
//! Usage: `cargo run --bin basic_scope`

use dcontext::{
    get_context_variable, initialize, push_scope, set_context_variable, RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct RequestId(String);

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct UserId(u64);

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<RequestId>("request_id");
    builder.register::<UserId>("user_id");
    initialize(builder);

    set_context_variable("request_id", RequestId("req-001".into()));
    set_context_variable("user_id", UserId(42));
    println!(
        "[root] request_id = {:?}",
        get_context_variable::<RequestId>("request_id").unwrap()
    );
    println!(
        "[root] user_id    = {:?}",
        get_context_variable::<UserId>("user_id").unwrap()
    );

    {
        let _guard = push_scope("request-child");
        set_context_variable("request_id", RequestId("req-002-child".into()));
        println!(
            "[child] request_id = {:?}",
            get_context_variable::<RequestId>("request_id").unwrap()
        );
        println!(
            "[child] user_id    = {:?}",
            get_context_variable::<UserId>("user_id").unwrap()
        );
    }
    println!(
        "[root] request_id = {:?}",
        get_context_variable::<RequestId>("request_id").unwrap()
    );

    {
        let _guard = push_scope("user-override");
        set_context_variable("user_id", UserId(99));
        println!(
            "[scope] user_id = {:?}",
            get_context_variable::<UserId>("user_id").unwrap()
        );
    }
    println!(
        "[root] user_id = {:?}",
        get_context_variable::<UserId>("user_id").unwrap()
    );
}
