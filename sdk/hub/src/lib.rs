mod protocol;

#[cfg(unix)]
mod client;

#[cfg(unix)]
pub use client::{Error, HubClient, Result, Stream};
pub use herdr_client::{AgentInfo, AgentStatus, TabInfo, WorkspaceInfo};
pub use protocol::{ClientMessage, HostState, Model, PROTOCOL, ServerMessage, SessionState};

#[cfg(unix)]
pub fn socket_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("HERDR_HUB_SOCKET_PATH").filter(|path| !path.is_empty()) {
        return path.into();
    }
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    format!("/tmp/herdr-hub-{uid}.sock").into()
}
