//! Live agent source: streams attention-ordered agent items to herdr-picker.

use std::process::ExitCode;

use herdr_hub_client::{AgentInfo, Model, SessionState, attention_order};
use herdr_picker_sdk::{Item, presentation, run, serve};
use serde_json::json;

fn main() -> ExitCode {
    run(env!("CARGO_BIN_NAME"), serve(agent_items))
}

fn agent_items(model: &Model) -> Vec<Item> {
    let mut agents = model
        .sessions
        .iter()
        .flat_map(|session| session.agents.iter().map(move |agent| (session, agent)))
        .collect::<Vec<_>>();
    agents.sort_by(|(_, a), (_, b)| attention_order(a, b));

    agents
        .into_iter()
        .map(|(session, agent)| agent_item(session, agent))
        .collect()
}

fn agent_item(session: &SessionState, agent: &AgentInfo) -> Item {
    let workspace = session
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)
        .map(|workspace| workspace.label.as_str())
        .unwrap_or(agent.workspace_id.as_str());
    let tab = session
        .tabs
        .iter()
        .find(|tab| tab.tab_id == agent.tab_id)
        .map(|tab| tab.label.as_str())
        .unwrap_or(agent.tab_id.as_str());
    let stripped_title = nonempty(agent.terminal_title_stripped.as_deref());
    let terminal_title = nonempty(agent.terminal_title.as_deref());
    let title = [
        nonempty(agent.name.as_deref()),
        stripped_title,
        terminal_title,
        Some(agent.terminal_id.as_str()),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| *candidate != workspace)
    .map(|candidate| format!("{workspace}: {candidate}"))
    .unwrap_or_else(|| workspace.into());
    let cwd = nonempty(agent.foreground_cwd.as_deref()).or_else(|| nonempty(agent.cwd.as_deref()));
    let kind = nonempty(agent.agent.as_deref());
    let subtitle = join_nonempty([cwd, kind]);
    let detail = join_nonempty([
        agent.name.as_deref(),
        agent.agent.as_deref(),
        Some(workspace),
        Some(tab),
        agent.title.as_deref(),
        stripped_title.or(terminal_title),
        cwd,
        Some(agent.agent_status.as_str()),
        Some(agent.terminal_id.as_str()),
    ]);
    let (indicator, tone, spinning) = presentation(&agent.agent_status);
    Item {
        id: format!("{}:{}", session.key, agent.pane_id),
        title,
        subtitle,
        detail,
        badge: if session.host == "local" {
            session.name.clone()
        } else {
            session.key.clone()
        },
        indicator: indicator.into(),
        tone: Some(tone),
        spinning,
        search: String::new(),
        value: json!({
            "pane_id": agent.pane_id,
            "workspace_id": agent.workspace_id,
            "tab_id": agent.tab_id,
            "session": session.key,
            "status": agent.agent_status,
            "name": agent.name,
            "agent": agent.agent,
        }),
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn join_nonempty<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> String {
    values
        .into_iter()
        .filter_map(nonempty)
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
    };

    use herdr_hub_client::{HostState, TabInfo, WorkspaceInfo};

    fn agent(pane_id: &str, status: &str, sequence: u64) -> AgentInfo {
        AgentInfo {
            terminal_id: format!("terminal-{pane_id}"),
            name: Some(pane_id.into()),
            agent: Some("codex".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: None,
            agent_status: status.into(),
            screen_detection_skipped: false,
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            agent_session: None,
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            pane_id: pane_id.into(),
            focused: false,
            launch_pending: false,
            interactive_ready: true,
            state_change_seq: sequence,
            cwd: Some("/project".into()),
            foreground_cwd: None,
            revision: 0,
        }
    }

    fn session(key: &str, host: &str, name: &str, agents: Vec<AgentInfo>) -> SessionState {
        SessionState {
            key: key.into(),
            host: host.into(),
            name: name.into(),
            connected: true,
            error: None,
            protocol: 19,
            workspaces: vec![WorkspaceInfo {
                workspace_id: "w1".into(),
                number: 1,
                label: "project".into(),
                focused: false,
                pane_count: agents.len(),
                tab_count: 1,
                active_tab_id: "t1".into(),
                agent_status: "idle".into(),
                tokens: HashMap::new(),
                worktree: None,
            }],
            tabs: vec![TabInfo {
                tab_id: "t1".into(),
                workspace_id: "w1".into(),
                number: 1,
                label: "agents".into(),
                focused: false,
                pane_count: agents.len(),
                agent_status: "idle".into(),
            }],
            agents,
            socket_path: Some(PathBuf::from("/tmp/herdr.sock")),
            client_focused: None,
        }
    }

    fn model(sessions: Vec<SessionState>) -> Model {
        Model {
            version: 1,
            active: None,
            hosts: vec![HostState {
                key: "local".into(),
                connected: true,
                error: None,
            }],
            sessions,
        }
    }

    #[test]
    fn agent_picker_prioritizes_attention_and_uses_generic_presentation() {
        let items = agent_items(&model(vec![session(
            "local/default",
            "local",
            "default",
            vec![
                agent("idle", "idle", 1),
                agent("blocked-old", "blocked", 2),
                agent("done", "done", 3),
                agent("working", "working", 4),
                agent("blocked-new", "blocked", 5),
            ],
        )]));
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            [
                "local/default:blocked-new",
                "local/default:blocked-old",
                "local/default:done",
                "local/default:working",
                "local/default:idle"
            ]
        );
        assert_eq!(items[0].tone, Some(herdr_picker_sdk::Tone::Danger));
        assert!(items[3].spinning);
        assert_eq!(items[0].value["pane_id"], "blocked-new");
        assert_eq!(items[0].value["session"], "local/default");
    }

    #[test]
    fn tab_label_is_not_used_as_the_agent_title() {
        let mut agent = agent("pane", "idle", 1);
        agent.name = None;
        agent.terminal_title_stripped = Some("shell".into());

        let item = agent_items(&model(vec![session(
            "local/default",
            "local",
            "default",
            vec![agent],
        )]))
        .remove(0);

        assert_eq!(item.title, "project: shell");
        assert!(item.detail.contains("agents"));
    }

    #[test]
    fn sessions_scope_ids_labels_badges_and_routing() {
        let mut local_agent = agent("same-pane", "idle", 1);
        local_agent.tab_id = "shared-tab".into();
        let mut remote_agent = agent("same-pane", "idle", 1);
        remote_agent.tab_id = "shared-tab".into();

        let mut local = session("local/main", "local", "main", vec![local_agent]);
        local.workspaces[0].label = "local project".into();
        local.tabs[0].tab_id = "shared-tab".into();
        local.tabs[0].label = "local tab".into();
        let mut remote = session("studio/main", "studio", "main", vec![remote_agent]);
        remote.workspaces[0].label = "remote project".into();
        remote.tabs[0].tab_id = "shared-tab".into();
        remote.tabs[0].label = "remote tab".into();

        let items = agent_items(&model(vec![local, remote]));
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["local/main:same-pane", "studio/main:same-pane"])
        );

        let local = items
            .iter()
            .find(|item| item.id.starts_with("local/"))
            .unwrap();
        assert_eq!(local.value["session"], "local/main");
        assert_eq!(local.badge, "main");
        assert!(local.title.starts_with("local project:"));
        assert!(local.detail.contains("local tab"));
        assert!(!local.detail.contains("remote project"));
        assert!(!local.detail.contains("remote tab"));

        let remote = items
            .iter()
            .find(|item| item.id.starts_with("studio/"))
            .unwrap();
        assert_eq!(remote.value["session"], "studio/main");
        assert_eq!(remote.badge, "studio/main");
        assert!(remote.title.starts_with("remote project:"));
        assert!(remote.detail.contains("remote tab"));
        assert!(!remote.detail.contains("local project"));
        assert!(!remote.detail.contains("local tab"));
    }

    #[test]
    fn attention_order_is_global_across_sessions() {
        let items = agent_items(&model(vec![
            session(
                "local/default",
                "local",
                "default",
                vec![agent("working-new", "working", 99)],
            ),
            session(
                "studio/default",
                "studio",
                "default",
                vec![
                    agent("blocked-old", "blocked", 1),
                    agent("done-new", "done", 200),
                ],
            ),
        ]));

        assert_eq!(
            items
                .iter()
                .map(|item| item.value["pane_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["blocked-old", "done-new", "working-new"]
        );
    }

    #[test]
    fn agent_kind_stays_searchable_outside_the_session_badge() {
        let item = agent_items(&model(vec![session(
            "local/default",
            "local",
            "default",
            vec![agent("pane", "idle", 1)],
        )]))
        .remove(0);

        assert_eq!(item.badge, "default");
        assert!(item.subtitle.contains("codex"));
        assert!(item.detail.contains("codex"));
    }
}
