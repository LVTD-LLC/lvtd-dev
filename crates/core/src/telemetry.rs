//! Structured logging. `RUST_LOG` controls the filter; the default keeps
//! our own crates at info and everything else quiet.

use tracing_subscriber::EnvFilter;

pub fn init(default_filter: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| default_filter.into()))
        .with_target(false)
        .compact()
        .init();
}
