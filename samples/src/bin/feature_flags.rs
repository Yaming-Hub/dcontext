//! # Sample 4: Feature Flags via Context
//!
//! Demonstrates using dcontext for feature flag propagation.
//! Feature flags are set once per request and available throughout the call
//! stack without parameter threading.
//!
//! Usage: `cargo run --bin feature_flags`

use dcontext::{initialize, sync_ctx, RegistryBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeatureFlags {
    dark_mode: bool,
    new_pricing: bool,
    beta_search: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            dark_mode: false,
            new_pricing: false,
            beta_search: false,
        }
    }
}

fn main() {
    let mut builder = RegistryBuilder::new();
    builder.register::<FeatureFlags>("features");
    initialize(builder);

    // Simulate per-request feature flag resolution.
    sync_ctx::set_context(
        "features",
        FeatureFlags {
            dark_mode: true,
            new_pricing: false,
            beta_search: true,
        },
    );

    render_page();

    // A/B test: override one flag for a sub-request.
    {
        let _guard = sync_ctx::enter_scope();
        let mut flags = sync_ctx::get_context::<FeatureFlags>("features").unwrap();
        flags.new_pricing = true;
        sync_ctx::set_context("features", flags);

        println!("\n--- A/B test scope ---");
        render_pricing();
    }

    // After scope: new_pricing is back to false.
    println!("\n--- After A/B scope ---");
    render_pricing();
}

fn render_page() {
    let flags = sync_ctx::get_context::<FeatureFlags>("features").unwrap();
    println!("Rendering page:");
    println!("  dark_mode   = {}", flags.dark_mode);
    println!("  beta_search = {}", flags.beta_search);
    render_pricing();
}

fn render_pricing() {
    let flags = sync_ctx::get_context::<FeatureFlags>("features").unwrap();
    if flags.new_pricing {
        println!("  pricing: showing NEW pricing model");
    } else {
        println!("  pricing: showing standard pricing");
    }
}
