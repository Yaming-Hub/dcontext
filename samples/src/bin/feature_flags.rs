//! # Sample 4: Feature Flags via Context
//!
//! Demonstrates using dcontext for feature flag propagation.
//!
//! Usage: `cargo run --bin feature_flags`

use dcontext::{
    get_context_variable, initialize, push_scope, set_context_variable, RegistryBuilder,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct FeatureFlags {
    dark_mode: bool,
    new_pricing: bool,
    beta_search: bool,
}

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<FeatureFlags>("features");
    initialize(builder);

    set_context_variable(
        "features",
        FeatureFlags {
            dark_mode: true,
            new_pricing: false,
            beta_search: true,
        },
    );

    render_page();

    {
        let _guard = push_scope("pricing-ab-test");
        let mut flags = get_context_variable::<FeatureFlags>("features").unwrap();
        flags.new_pricing = true;
        set_context_variable("features", flags);

        println!("\n--- A/B test scope ---");
        render_pricing();
    }

    println!("\n--- After A/B scope ---");
    render_pricing();
}

fn render_page() {
    let flags = get_context_variable::<FeatureFlags>("features").unwrap();
    println!("Rendering page:");
    println!("  dark_mode   = {}", flags.dark_mode);
    println!("  beta_search = {}", flags.beta_search);
    render_pricing();
}

fn render_pricing() {
    let flags = get_context_variable::<FeatureFlags>("features").unwrap();
    if flags.new_pricing {
        println!("  pricing: showing NEW pricing model");
    } else {
        println!("  pricing: showing standard pricing");
    }
}
