use std::path::PathBuf;

use herdr_client::{AgentInfo, TabInfo, WorkspaceInfo};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub version: u64,
    pub active: Option<String>,
    pub hosts: Vec<HostState>,
    pub sessions: Vec<SessionState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostState {
    pub key: String,
    pub connected: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionState {
    pub key: String,
    pub host: String,
    pub name: String,
    pub connected: bool,
    pub error: Option<String>,
    pub protocol: u32,
    pub workspaces: Vec<WorkspaceInfo>,
    pub tabs: Vec<TabInfo>,
    pub agents: Vec<AgentInfo>,
    pub socket_path: Option<PathBuf>,
    #[serde(deserialize_with = "required_option")]
    pub client_focused: Option<bool>,
}

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Subscribe {
        protocol: u32,
    },
    Call {
        protocol: u32,
        id: u64,
        session: String,
        method: String,
        params: Value,
    },
    Notify {
        protocol: u32,
        socket_path: PathBuf,
        event: Option<Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessage {
    Hello { protocol: u32, model: Model },
    Session { version: u64, session: SessionState },
    SessionRemoved { version: u64, key: String },
    Host { version: u64, host: HostState },
    Active { version: u64, key: Option<String> },
    Reply { id: u64, result: Value },
    ReplyError { id: u64, error: String },
    Error { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_messages_round_trip() {
        let client = ClientMessage::Call {
            protocol: PROTOCOL,
            id: 7,
            session: "local/default".into(),
            method: "pane.focus".into(),
            params: serde_json::json!({ "pane_id": "w1:p1" }),
        };
        let encoded = serde_json::to_vec(&client).unwrap();
        assert_eq!(
            serde_json::from_slice::<ClientMessage>(&encoded).unwrap(),
            client
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&encoded).unwrap()["type"],
            "call"
        );

        let server = ServerMessage::Hello {
            protocol: PROTOCOL,
            model: Model {
                version: 4,
                active: Some("local/default".into()),
                hosts: vec![HostState {
                    key: "local".into(),
                    connected: true,
                    error: None,
                }],
                sessions: Vec::new(),
            },
        };
        let encoded = serde_json::to_vec(&server).unwrap();
        assert_eq!(
            serde_json::from_slice::<ServerMessage>(&encoded).unwrap(),
            server
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&encoded).unwrap()["type"],
            "hello"
        );
    }

    #[test]
    fn tagged_messages_reject_unknown_fields() {
        let value = serde_json::json!({
            "type": "subscribe",
            "protocol": PROTOCOL,
            "extra": true,
        });
        assert!(serde_json::from_value::<ClientMessage>(value).is_err());
    }

    #[test]
    fn session_tabs_are_required() {
        let value = serde_json::json!({
            "key": "local/default",
            "host": "local",
            "name": "default",
            "connected": true,
            "error": null,
            "protocol": 20,
            "workspaces": [],
            "agents": [],
            "socket_path": null,
            "client_focused": null,
        });
        assert!(serde_json::from_value::<SessionState>(value).is_err());
    }

    #[test]
    fn session_focus_is_required_and_terminal_is_rejected() {
        let missing = serde_json::json!({
            "key": "local/default",
            "host": "local",
            "name": "default",
            "connected": true,
            "error": null,
            "protocol": 20,
            "workspaces": [],
            "tabs": [],
            "agents": [],
            "socket_path": null,
        });
        assert!(serde_json::from_value::<SessionState>(missing).is_err());

        let legacy = serde_json::json!({
            "key": "local/default",
            "host": "local",
            "name": "default",
            "connected": true,
            "error": null,
            "protocol": 20,
            "workspaces": [],
            "tabs": [],
            "agents": [],
            "socket_path": null,
            "client_focused": true,
            "terminal": "ghostty-1",
        });
        assert!(serde_json::from_value::<SessionState>(legacy).is_err());
    }
}
