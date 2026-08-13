//! Thin client primitives for Herdr plugins.
//!
//! The library intentionally mirrors the public socket protocol rather than
//! Herdr's internal Rust types. Unknown JSON fields are ignored and raw calls
//! remain available so a newer Herdr method does not require a client release.

mod client;
mod env;
mod types;

pub use client::{ApiError, Client, Error, Subscription};
pub use env::{
    Environment, EnvironmentError, PluginEnvironmentError, PluginInvocation, PluginPaths,
    open_rotating_log, socket_scope_dir,
};
pub use types::*;

/// Stable Herdr release used to validate the typed models.
pub const TESTED_HERDR_VERSION: &str = "0.8.0";
/// Socket protocol shipped by [`TESTED_HERDR_VERSION`].
pub const TESTED_PROTOCOL: u32 = 20;
