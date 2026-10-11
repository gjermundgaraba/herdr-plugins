//! Thin client primitives for Herdr plugins.
//!
//! The library intentionally mirrors the public socket protocol rather than
//! Herdr's internal Rust types. Unknown JSON fields are ignored and raw calls
//! remain available so a newer Herdr method does not require a client release.

mod client;
mod clients;
mod env;
#[cfg(unix)]
pub mod follow;
pub mod hash;
pub mod ndjson;
#[cfg(unix)]
pub mod sessions;
pub mod slots;
mod types;
#[cfg(unix)]
pub mod unix;

pub use client::{ApiError, Client, Error, PendingJump, Subscription};
pub use clients::{
    CLIENT_JUMP_TIMEOUT, ClientAgent, ClientChanged, ClientEndpointStatus, ClientInfo,
    ClientJumpError, ClientJumpParams, ClientJumped, ClientList, ClientReport, ClientReportAgent,
    ClientReportEndpoint,
};
pub use env::{
    Environment, EnvironmentError, PluginContext, PluginEnvironmentError, PluginInvocation,
    PluginPaths, open_rotating_log, socket_scope_dir,
};
pub use types::*;
