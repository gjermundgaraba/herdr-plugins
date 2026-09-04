//! Live workspace source: streams workspace items to herdr-picker.

use std::process::ExitCode;

use herdr_hub_client::{Model, SessionState};
use herdr_picker_sdk::{Item, presentation, run, serve};
use serde_json::json;

fn main() -> ExitCode {
    run(env!("CARGO_BIN_NAME"), serve(workspace_items))
}

fn workspace_items(model: &Model) -> Vec<Item> {
    model
        .sessions
        .iter()
        .filter(|session| session.connected)
        .flat_map(|session| {
            let mut workspaces = session.workspaces.iter().collect::<Vec<_>>();
            workspaces.sort_by_key(|workspace| workspace.number);
            workspaces.into_iter().map(move |workspace| {
                let (indicator, tone, spinning) = presentation(&workspace.agent_status);
                Item {
                    id: format!("{}:{}", session.key, workspace.workspace_id),
                    title: workspace.label.clone(),
                    subtitle: format!(
                        "{} tabs · {} panes · {}",
                        workspace.tab_count, workspace.pane_count, workspace.agent_status
                    ),
                    detail: workspace
                        .worktree
                        .as_ref()
                        .map(|worktree| {
                            format!("{} · {}", workspace.workspace_id, worktree.checkout_path)
                        })
                        .unwrap_or_else(|| workspace.workspace_id.clone()),
                    badge: session_badge(session).into(),
                    indicator: indicator.into(),
                    tone: Some(tone),
                    spinning,
                    search: String::new(),
                    value: json!({
                        "session": session.key,
                        "workspace_id": workspace.workspace_id,
                        "number": workspace.number,
                    }),
                }
            })
        })
        .collect()
}

fn session_badge(session: &SessionState) -> &str {
    if session.host == "local" {
        &session.name
    } else {
        &session.key
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_hub_client::{HostState, WorkspaceInfo};
    use std::collections::HashMap;

    fn workspace(workspace_id: &str, number: usize, label: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: workspace_id.into(),
            number,
            label: label.into(),
            focused: false,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: "t1".into(),
            agent_status: "idle".into(),
            tokens: HashMap::new(),
            worktree: None,
        }
    }

    fn session(
        key: &str,
        host: &str,
        name: &str,
        connected: bool,
        workspaces: Vec<WorkspaceInfo>,
    ) -> SessionState {
        SessionState {
            key: key.into(),
            host: host.into(),
            name: name.into(),
            connected,
            error: None,
            protocol: 20,
            workspaces,
            tabs: Vec::new(),
            agents: Vec::new(),
            socket_path: None,
            client_focused: None,
        }
    }

    #[test]
    fn workspace_picker_flattens_connected_sessions_with_composite_routing() {
        let model = Model {
            version: 1,
            active: None,
            hosts: vec![
                HostState {
                    key: "local".into(),
                    connected: true,
                    error: None,
                },
                HostState {
                    key: "workbox".into(),
                    connected: true,
                    error: None,
                },
            ],
            sessions: vec![
                session(
                    "local/default",
                    "local",
                    "default",
                    true,
                    vec![
                        workspace("shared", 2, "second"),
                        workspace("first", 1, "project"),
                    ],
                ),
                session(
                    "workbox/default",
                    "workbox",
                    "default",
                    true,
                    vec![workspace("shared", 1, "remote")],
                ),
                session(
                    "local/offline",
                    "local",
                    "offline",
                    false,
                    vec![workspace("hidden", 1, "hidden")],
                ),
            ],
        };

        let items = workspace_items(&model);
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            [
                "local/default:first",
                "local/default:shared",
                "workbox/default:shared",
            ]
        );
        assert_eq!(items[0].badge, "default");
        assert_eq!(items[2].badge, "workbox/default");
        assert_eq!(items[1].value["session"], "local/default");
        assert_eq!(items[1].value["workspace_id"], "shared");
        assert_eq!(items[2].value["session"], "workbox/default");
    }
}
