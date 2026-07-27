//! Client for herdr's newline-delimited JSON API over `$HERDR_SOCKET_PATH`.
//!
//! One request per line, one response per line — and one request per
//! CONNECTION (herdr hangs up after answering), so every call dials fresh:
//!   -> {"id":"1","method":"session.snapshot","params":{}}
//!   <- {"id":"1","result":{"type":"session_snapshot","snapshot":{...}}}
//!   <- {"id":"1","error":{"code":"...","message":"..."}}
//!
//! The structs model only the fields the picker uses; serde ignores unknown
//! fields, so herdr adding fields never breaks us.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

/// Mirror of herdr's internal `AgentState`. The socket collapses
/// `(AgentState, seen)` into one `agent_status`; `split` re-expands it at
/// this boundary so every copied `match (state, seen)` stays verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

/// Inverse of herdr's `pane_agent_status`: done = Idle && !seen.
pub fn split(status: AgentStatus) -> (AgentState, bool) {
    match status {
        AgentStatus::Blocked => (AgentState::Blocked, true),
        AgentStatus::Working => (AgentState::Working, true),
        AgentStatus::Done => (AgentState::Idle, false),
        AgentStatus::Idle => (AgentState::Idle, true),
        AgentStatus::Unknown => (AgentState::Unknown, true),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub state_labels: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentInfo {
    pub pane_id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Terminal title with the agent's status glyphs stripped — for coding
    /// agents this is the live task summary (what the native sidebar shows).
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Working directory of the foreground process — where the agent
    /// actually is, when it differs from the pane's starting cwd.
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    /// Monotonic session counter bumped when the agent changes state — herdr's
    /// own recency ordering (higher = changed more recently). Not a timestamp,
    /// and it resets when the herdr server restarts.
    #[serde(default)]
    pub state_change_seq: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub tabs: Vec<TabInfo>,
    pub panes: Vec<PaneInfo>,
    #[serde(default)]
    pub agents: Vec<AgentInfo>,
}

/// Builds one request line (without the trailing newline). herdr's Method
/// enum is serde tag="method"/content="params": params must be present even
/// when empty.
fn request_line(id: &str, method: &str, params: Value) -> String {
    json!({"id": id, "method": method, "params": params}).to_string()
}

/// Validates the response envelope for `expected_id` and unwraps `result`.
/// Errors first: herdr answers requests it cannot parse with an EMPTY id,
/// and that error message beats an "id mismatch" complaint every time.
fn parse_response(line: &str, expected_id: &str) -> Result<Value> {
    let envelope: Value =
        serde_json::from_str(line).context("herdr sent a response that is not valid JSON")?;
    if let Some(error) = envelope.get("error") {
        let code = error["code"].as_str().unwrap_or("unknown_error");
        let message = error["message"].as_str().unwrap_or("no message");
        bail!("herdr error {code}: {message}");
    }
    let id = envelope["id"].as_str().unwrap_or_default();
    if id != expected_id {
        bail!("herdr response id {id:?} does not match request id {expected_id:?}");
    }
    match envelope.get("result") {
        Some(result) => Ok(result.clone()),
        None => bail!("herdr response carries neither result nor error"),
    }
}

/// herdr's API server answers exactly ONE request per connection, so the
/// client dials a fresh connection for every call.
#[derive(Debug)]
pub struct SocketClient {
    socket_path: std::path::PathBuf,
    next_id: u64,
}

impl SocketClient {
    pub fn connect(socket_path: &Path) -> Result<Self> {
        // Probe now so a dead socket fails at startup with a clear message.
        Self::dial(socket_path)?;
        Ok(SocketClient {
            socket_path: socket_path.to_path_buf(),
            next_id: 1,
        })
    }

    fn dial(socket_path: &Path) -> Result<UnixStream> {
        UnixStream::connect(socket_path).with_context(|| {
            format!(
                "cannot connect to the herdr API socket at {}; \
                 the picker must run inside a herdr session",
                socket_path.display()
            )
        })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.to_string();
        self.next_id += 1;

        let mut reader = BufReader::new(Self::dial(&self.socket_path)?);
        let line = request_line(&id, method, params);
        let stream = reader.get_mut();
        stream
            .write_all(line.as_bytes())
            .and_then(|_| stream.write_all(b"\n"))
            .with_context(|| format!("failed to send {method} request to herdr"))?;

        let mut response = String::new();
        let read = reader
            .read_line(&mut response)
            .with_context(|| format!("failed to read herdr's {method} response"))?;
        if read == 0 {
            bail!("herdr closed the connection before answering {method}");
        }
        parse_response(&response, &id)
    }

    pub fn snapshot(&mut self) -> Result<SessionSnapshot> {
        let result = self.call("session.snapshot", json!({}))?;
        let actual_type = result["type"].as_str().unwrap_or("<missing>");
        if actual_type != "session_snapshot" {
            bail!("expected a session_snapshot result from herdr, got {actual_type}");
        }
        serde_json::from_value(result["snapshot"].clone())
            .context("could not parse snapshot out of a session_snapshot result")
    }

    pub fn focus_pane(&mut self, pane_id: &str) -> Result<()> {
        // Any success result means the focus landed.
        self.call("pane.focus", json!({"pane_id": pane_id}))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_reexpands_done_as_unseen_idle() {
        assert_eq!(split(AgentStatus::Done), (AgentState::Idle, false));
        assert_eq!(split(AgentStatus::Idle), (AgentState::Idle, true));
        assert_eq!(split(AgentStatus::Blocked), (AgentState::Blocked, true));
    }

    #[test]
    fn error_wins_over_id_mismatch() {
        let err = parse_response(r#"{"id":"","error":{"code":"bad","message":"nope"}}"#, "7")
            .unwrap_err();
        assert!(err.to_string().contains("bad"), "{err}");
    }

    #[test]
    fn snapshot_payload_parses_with_minimal_fields() {
        let raw = r#"{
            "focused_pane_id": "w1:p2",
            "workspaces": [{"workspace_id":"w1","label":"api","focused":true}],
            "tabs": [{"tab_id":"w1:t1","workspace_id":"w1","label":"main"}],
            "panes": [{"pane_id":"w1:p2","workspace_id":"w1","tab_id":"w1:t1",
                       "focused":true,"agent":"claude","agent_status":"done"}],
            "agents": [{"pane_id":"w1:p2","terminal_id":"x","name":"fixer"}]
        }"#;
        let snap: SessionSnapshot = serde_json::from_str(raw).unwrap();
        assert_eq!(snap.panes[0].agent_status, AgentStatus::Done);
        assert_eq!(snap.agents[0].name.as_deref(), Some("fixer"));
    }
}
