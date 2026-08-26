//! Thin client primitives for Herdr plugins.
//!
//! The library intentionally mirrors the public socket protocol rather than
//! Herdr's internal Rust types. Unknown JSON fields are ignored and raw calls
//! remain available so a newer Herdr method does not require a client release.

mod client;
mod env;
pub mod hash;
pub mod ndjson;
mod types;
#[cfg(unix)]
pub mod unix;

pub use client::{ApiError, Client, Error, Subscription};
pub use env::{
    Environment, EnvironmentError, PluginEnvironmentError, PluginInvocation, PluginPaths,
    open_rotating_log, socket_scope_dir,
};
pub use types::*;
