//! Attached clients: the Agents list each client reports to its daemon
//! (`client.list`, announced by `client.changed`) and the jump a tool asks
//! one of them to perform (`client.jump`).
//!
//! These mirror the Herdr fork's client contract; `tests/client_contract.rs`
//! decodes the fork's canonical fixtures so drift fails a test.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{AgentStatus, ApiError, Error, EventEnvelope, EventSubscription};

/// Socket timeout for `client.jump`: longer than the twenty seconds a daemon
/// waits for its client, so the daemon's typed answer arrives before this
/// client gives up.
pub const CLIENT_JUMP_TIMEOUT: Duration = Duration::from_secs(25);

/// Forward-compatible endpoint status. Compare with the associated constants.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientEndpointStatus(pub String);

impl ClientEndpointStatus {
    pub const ONLINE: &'static str = "online";
    pub const RECONNECTING: &'static str = "reconnecting";

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClientEndpointStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ClientEndpointStatus {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// What a client shows in its Agents list, across every endpoint it is
/// attached to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientReport {
    /// Whether the client's OS window or outer terminal has focus; `None`
    /// until the client knows.
    pub focused: Option<bool>,
    /// The endpoint the client is presenting.
    pub active_endpoint_id: Option<String>,
    pub endpoints: Vec<ClientReportEndpoint>,
    /// The Agents list in the client's display order.
    pub agents: Vec<ClientReportAgent>,
}

impl ClientReport {
    pub fn endpoint(&self, endpoint_id: &str) -> Option<&ClientReportEndpoint> {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == endpoint_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientReportEndpoint {
    /// The client's own id for the endpoint, such as `local` or `ssh:<profile>`.
    pub endpoint_id: String,
    pub label: String,
    pub status: ClientEndpointStatus,
    /// Boot of the daemon behind the endpoint; `None` before its first snapshot.
    pub boot_id: Option<String>,
}

impl ClientReportEndpoint {
    /// The boot a jump to this endpoint must carry: present only while the
    /// endpoint is online and has reported a snapshot.
    pub fn jump_boot_id(&self) -> Option<&str> {
        (self.status.as_str() == ClientEndpointStatus::ONLINE)
            .then_some(self.boot_id.as_deref())
            .flatten()
            .filter(|boot_id| !boot_id.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientReportAgent {
    pub endpoint_id: String,
    /// The one agent the client has focused; `agent.focused` is instead the
    /// focus inside the row's own endpoint.
    pub focused: bool,
    /// Label and number of the space the agent belongs to.
    pub workspace_label: String,
    pub workspace_number: usize,
    pub cwd: Option<String>,
    pub foreground_cwd: Option<String>,
    /// The row's record from its endpoint's snapshot, as the client shows it.
    pub agent: ClientAgent,
}

/// An agent record of an endpoint snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientAgent {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub name: Option<String>,
    pub display_agent: Option<String>,
    pub agent: Option<String>,
    pub title: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_status: AgentStatus,
    pub state_change_seq: u64,
    pub state_labels: Vec<(String, String)>,
    pub tokens: Vec<(String, String)>,
    /// Focus inside the agent's own endpoint.
    pub focused: bool,
    /// The agent's place in the daemon's `priority` order. Opaque: compare
    /// keys byte by byte, smallest first. Empty from a daemon without it.
    #[serde(default)]
    pub priority_key: String,
    #[serde(default)]
    pub priority_mark: Option<String>,
}

/// One attached client, as `client.list` describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub client_id: u64,
    /// The client's latest report; absent until it sends one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<ClientReport>,
}

/// The `client.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientList {
    /// The answering daemon's own boot.
    pub boot_id: String,
    pub clients: Vec<ClientInfo>,
}

/// The `client.changed` event: a client attached, sent a different report,
/// or detached. It names the client only; read `client.list` for its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientChanged {
    pub client_id: u64,
}

impl ClientChanged {
    /// The name `events.subscribe` takes.
    pub const SUBSCRIPTION: &'static str = "client.changed";
    /// The discriminator on pushed event envelopes.
    pub const EVENT: &'static str = "client_changed";

    pub fn subscription() -> EventSubscription {
        EventSubscription::new(Self::SUBSCRIPTION)
    }

    /// Decodes a pushed envelope; `None` for any other event.
    pub fn from_event(event: &EventEnvelope) -> Option<Result<Self, serde_json::Error>> {
        (event.event == Self::EVENT).then(|| Self::deserialize(&event.data))
    }
}

/// Asks one client to show a pane, switching endpoint when it has to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientJumpParams {
    pub client_id: u64,
    /// The endpoint, in the ids of that client's report.
    pub endpoint_id: String,
    /// The endpoint's boot the pane id was read from.
    pub boot_id: String,
    pub pane_id: String,
}

/// The `client.jump` result: the client presents the endpoint with the pane
/// focused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientJumped {
    pub client_id: u64,
    pub endpoint_id: String,
    pub pane_id: String,
}

/// Why `client.jump` did not report the pane focused.
#[derive(Debug, thiserror::Error)]
pub enum ClientJumpError {
    #[error("no client with that id is attached")]
    ClientNotFound,
    #[error("that client is attached but sends no report, so it does not answer jumps")]
    ClientNotReporting,
    /// A newer jump to the same client replaced this one. Expected when a
    /// second press follows the first; not a failure of the newer jump.
    #[error("a newer jump to that client replaced this one")]
    Superseded,
    #[error("the client detached before it answered; the jump may still have happened")]
    ClientDeparted,
    #[error("the endpoint now runs another boot")]
    StaleBoot,
    #[error("the client knows no such endpoint or pane")]
    TargetNotFound,
    /// The endpoint is not online, or has sent the client no snapshot yet.
    /// When it went away while the client was navigating there, the jump may
    /// still have happened.
    #[error("the client could not reach the endpoint; the jump may still have happened")]
    EndpointUnavailable,
    #[error("the client did not confirm the jump in time; it may still have happened")]
    Timeout,
    /// Any other API error, or a transport failure. After a transport
    /// failure the jump may still have happened.
    #[error("{0}")]
    Other(#[source] Error),
}

impl From<Error> for ClientJumpError {
    fn from(error: Error) -> Self {
        let Error::Api(ApiError { code, message }) = error else {
            return Self::Other(error);
        };
        match code.as_str() {
            "client_not_found" => Self::ClientNotFound,
            "client_not_reporting" => Self::ClientNotReporting,
            "jump_superseded" => Self::Superseded,
            "client_departed" => Self::ClientDeparted,
            "stale_boot" => Self::StaleBoot,
            "jump_target_not_found" => Self::TargetNotFound,
            "jump_endpoint_unavailable" => Self::EndpointUnavailable,
            "jump_timeout" => Self::Timeout,
            _ => Self::Other(Error::Api(ApiError { code, message })),
        }
    }
}
