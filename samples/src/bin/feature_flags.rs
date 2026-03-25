//! # Sample 4: Feature Flags via Context
//!
//! Demonstrates using dcontext for feature flag propagation.
//! Feature flags are set once per request and available throughout the call
//! stack without parameter threading.
//!
//! Usage: `cargo run --bin feature_flags`

use dcontext::{register, initialize, set_context, get_context, scope};
use serde::{Serialize, Deserialize};

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
    register::<FeatureFlags>("features");
    initialize();

    // Simulate per-request feature flag resolution.
    set_context("features", FeatureFlags {
        dark_mode: true,
        new_pricing: false,
        beta_search: true,
    });

    render_page();

    // A/B test: override one flag for a sub-request.
    scope(|| {
        let mut flags = get_context::<FeatureFlags>("features");
        flags.new_pricing = true;
        set_context("features", flags);

        println!("\n--- A/B test scope ---");
        render_pricing();
    });

    // After scope: new_pricing is back to false.
    println!("\n--- After A/B scope ---");
    render_pricing();
}

fn render_page() {
    let flags = get_context::<FeatureFlags>("features");
    println!("Rendering page:");
    println!("  dark_mode   = {}", flags.dark_mode);
    println!("  beta_search = {}", flags.beta_search);
    render_pricing();
}

fn render_pricing() {
    let flags = get_context::<FeatureFlags>("features");
    if flags.new_pricing {
        println!("  pricing: showing NEW pricing model");
    } else {
        println!("  pricing: showing standard pricing");
    }
}
