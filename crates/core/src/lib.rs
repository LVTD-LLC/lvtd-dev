//! core: the plumbing every other crate builds on.
//!
//! - [`blocking`]  bounded CPU work on the blocking pool
//! - [`config`]    env → `Config`
//! - [`db`]        Postgres pool; applies the migrations the app embeds
//! - [`error`]     `AppError`, the one error type handlers return
//! - [`http`]      security headers, same-origin guard, content negotiation
//! - [`jobs`]      durable background jobs in Postgres, worker and enqueue API
//! - [`mcp`]       Model Context Protocol server plumbing: stateless HTTP transport, JSON-RPC, tool results
//! - `og`          social card PNGs for link previews (`og` feature)
//! - [`storage`]   user uploads: an S3-compatible bucket when configured, a local directory otherwise
//! - [`telemetry`] tracing setup
//!
//! Nothing in here knows about users, sessions or pages.

/// This crate's tracing target, independent of the dependency alias.
pub const LOG_TARGET: &str = env!("CARGO_CRATE_NAME");

pub mod blocking;
pub mod config;
pub mod db;
pub mod error;
pub mod http;
pub mod jobs;
pub mod mcp;
#[cfg(feature = "og")]
pub mod og;
pub mod public_page;
pub mod storage;
pub mod telemetry;

pub use config::Config;
pub use db::Pool;
pub use error::AppError;

pub mod indexnow;
