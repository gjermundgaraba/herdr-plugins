mod protocol;

#[cfg(unix)]
mod client;
mod presentation;

#[cfg(unix)]
pub use client::{Error, HubClient, Result, Stream};
pub use herdr_client::{AgentInfo, AgentStatus, TabInfo, WorkspaceInfo};
pub use presentation::{attention_order, attention_rank};
pub use protocol::{ClientMessage, HostState, Model, PROTOCOL, ServerMessage, SessionState};

#[cfg(unix)]
pub fn socket_path() -> std::path::PathBuf {
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    format!("/tmp/herdr-hub-{uid}.sock").into()
}
