use std::path::PathBuf;

use herdr_client::{AgentInfo, TabInfo, WorkspaceInfo};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL: u32 = 6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub version: u64,
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
                hosts: vec![HostState {
                    key: "local".into(),
                    connected: true,
                    error: None,
                }],
                sessions: vec![SessionState {
                    key: "local/default".into(),
                    host: "local".into(),
                    name: "default".into(),
                    connected: true,
                    error: None,
                    protocol: 20,
                    workspaces: Vec::new(),
                    tabs: Vec::new(),
                    agents: Vec::new(),
                    socket_path: None,
                }],
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
}
