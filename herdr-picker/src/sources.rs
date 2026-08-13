use std::{
    cmp::Reverse,
    collections::HashMap,
    sync::mpsc::{Receiver, channel},
    thread,
};

use herdr_client::{AgentStatus, Client, SessionSnapshot};
use serde_json::json;

use crate::model::{Dispatch, Item, Kind};

pub type SourceUpdate = Result<Vec<Item>, String>;

pub fn spawn(client: Client) -> Receiver<SourceUpdate> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let items = client
            .snapshot()
            .map(|snapshot| {
                let mut items = workspaces(&snapshot);
                items.extend(agents(&snapshot));
                items.extend(tabs(&snapshot));
                items.extend(panes(&snapshot));
                items
            })
            .map_err(|error| error.to_string());
        let _ = tx.send(items);
    });
    rx
}

fn workspaces(snapshot: &SessionSnapshot) -> Vec<Item> {
    let mut workspaces = snapshot.workspaces.clone();
    workspaces.sort_by_key(|workspace| workspace.number);
    workspaces
        .into_iter()
        .map(|workspace| Item {
            kind: Kind::Workspace,
            agent_status: None,
            title: workspace.label.clone(),
            subtitle: format!(
                "{} tabs · {} panes · {}",
                workspace.tab_count, workspace.pane_count, workspace.agent_status
            ),
            detail: workspace.workspace_id.clone(),
            dispatch: Dispatch::new(
                "workspace.focus",
                json!({ "workspace_id": workspace.workspace_id }),
                &snapshot.session_epoch,
            ),
        })
        .collect()
}

fn tabs(snapshot: &SessionSnapshot) -> Vec<Item> {
    let workspaces: HashMap<&str, &str> = snapshot
        .workspaces
        .iter()
        .map(|workspace| (workspace.workspace_id.as_str(), workspace.label.as_str()))
        .collect();
    let mut tabs = snapshot.tabs.clone();
    tabs.sort_by_key(|tab| {
        (
            snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == tab.workspace_id)
                .map(|workspace| workspace.number)
                .unwrap_or(usize::MAX),
            tab.number,
        )
    });
    tabs.into_iter()
        .map(|tab| {
            let workspace = workspaces
                .get(tab.workspace_id.as_str())
                .copied()
                .unwrap_or(tab.workspace_id.as_str());
            Item {
                kind: Kind::Tab,
                agent_status: None,
                title: tab.label.clone(),
                subtitle: format!(
                    "{workspace} · {} panes · {}",
                    tab.pane_count, tab.agent_status
                ),
                detail: tab.tab_id.clone(),
                dispatch: Dispatch::new(
                    "tab.focus",
                    json!({ "tab_id": tab.tab_id }),
                    &snapshot.session_epoch,
                ),
            }
        })
        .collect()
}

fn panes(snapshot: &SessionSnapshot) -> Vec<Item> {
    let workspace_labels: HashMap<&str, &str> = snapshot
        .workspaces
        .iter()
        .map(|workspace| (workspace.workspace_id.as_str(), workspace.label.as_str()))
        .collect();
    let tab_labels: HashMap<&str, &str> = snapshot
        .tabs
        .iter()
        .map(|tab| (tab.tab_id.as_str(), tab.label.as_str()))
        .collect();
    snapshot
        .panes
        .iter()
        .map(|pane| {
            let workspace = workspace_labels
                .get(pane.workspace_id.as_str())
                .copied()
                .unwrap_or(pane.workspace_id.as_str());
            let tab = tab_labels
                .get(pane.tab_id.as_str())
                .copied()
                .unwrap_or(pane.tab_id.as_str());
            let title = pane
                .title
                .as_deref()
                .or(pane.label.as_deref())
                .or(pane.display_agent.as_deref())
                .or(pane.agent.as_deref())
                .or(pane.terminal_title_stripped.as_deref())
                .unwrap_or(pane.pane_id.as_str());
            let cwd = pane
                .foreground_cwd
                .as_deref()
                .or(pane.cwd.as_deref())
                .unwrap_or("");
            Item {
                kind: Kind::Pane,
                agent_status: None,
                title: title.into(),
                subtitle: format!("{workspace} · {tab} · {}", pane.agent_status),
                detail: format!("{} · {} · {cwd}", pane.pane_id, pane.terminal_id),
                dispatch: Dispatch::new(
                    "pane.focus",
                    json!({ "pane_id": pane.pane_id }),
                    &snapshot.session_epoch,
                ),
            }
        })
        .collect()
}

fn agents(snapshot: &SessionSnapshot) -> Vec<Item> {
    let workspace_labels: HashMap<&str, &str> = snapshot
        .workspaces
        .iter()
        .map(|workspace| (workspace.workspace_id.as_str(), workspace.label.as_str()))
        .collect();
    let tab_labels: HashMap<&str, &str> = snapshot
        .tabs
        .iter()
        .map(|tab| (tab.tab_id.as_str(), tab.label.as_str()))
        .collect();
    let explicit_tab_labels = explicit_tab_labels(snapshot);

    let mut agents: Vec<_> = snapshot.agents.iter().collect();
    agents.sort_by_key(|agent| {
        (
            agent_status_priority(&agent.agent_status),
            Reverse(agent.state_change_seq),
        )
    });

    agents
        .into_iter()
        .map(|agent| {
            let workspace = workspace_labels
                .get(agent.workspace_id.as_str())
                .copied()
                .unwrap_or(agent.workspace_id.as_str());
            let tab = tab_labels
                .get(agent.tab_id.as_str())
                .copied()
                .unwrap_or(agent.tab_id.as_str());
            let explicit_tab = explicit_tab_labels.get(agent.tab_id.as_str()).copied();
            let stripped_terminal_title = nonempty(agent.terminal_title_stripped.as_deref());
            let terminal_title = nonempty(agent.terminal_title.as_deref());
            let base_title = [
                nonempty(agent.name.as_deref()),
                explicit_tab,
                stripped_terminal_title,
                terminal_title,
                Some(agent.terminal_id.as_str()),
            ]
            .into_iter()
            .flatten()
            .find(|candidate| *candidate != workspace);
            let title = base_title
                .map(|base_title| format!("{workspace}: {base_title}"))
                .unwrap_or_else(|| workspace.into());
            let cwd = nonempty(agent.foreground_cwd.as_deref())
                .or_else(|| nonempty(agent.cwd.as_deref()));
            let subtitle = match (cwd, nonempty(agent.agent.as_deref())) {
                (Some(path), Some(kind)) => format!("{path} · {kind}"),
                (Some(path), None) => path.into(),
                (None, Some(kind)) => kind.into(),
                (None, None) => String::new(),
            };
            Item {
                kind: Kind::Agent,
                agent_status: Some(agent.agent_status.clone()),
                title,
                subtitle,
                detail: [
                    agent.name.as_deref().unwrap_or(""),
                    agent.agent.as_deref().unwrap_or(""),
                    workspace,
                    tab,
                    agent.title.as_deref().unwrap_or(""),
                    stripped_terminal_title.or(terminal_title).unwrap_or(""),
                    cwd.unwrap_or(""),
                    agent.agent_status.as_str(),
                    agent.terminal_id.as_str(),
                ]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
                dispatch: Dispatch::new(
                    "agent.focus",
                    json!({ "target": agent.pane_id }),
                    &snapshot.session_epoch,
                ),
            }
        })
        .collect()
}

fn explicit_tab_labels(snapshot: &SessionSnapshot) -> HashMap<&str, &str> {
    let mut positions = HashMap::new();
    snapshot
        .tabs
        .iter()
        .filter_map(|tab| {
            let position = positions.entry(tab.workspace_id.as_str()).or_insert(0usize);
            *position += 1;
            (!tab.label.trim().is_empty() && tab.label != position.to_string())
                .then_some((tab.tab_id.as_str(), tab.label.as_str()))
        })
        .collect()
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn agent_status_priority(status: &AgentStatus) -> u8 {
    match status.as_str() {
        AgentStatus::BLOCKED => 0,
        AgentStatus::DONE => 1,
        AgentStatus::WORKING => 2,
        AgentStatus::IDLE => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_client::{AgentInfo, TabInfo, WorkspaceInfo};

    fn agent(pane_id: &str, status: &str, state_change_seq: u64) -> AgentInfo {
        AgentInfo {
            terminal_id: format!("terminal-{pane_id}"),
            name: Some(pane_id.into()),
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: None,
            agent_status: status.into(),
            screen_detection_skipped: false,
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            agent_session: None,
            workspace_id: "workspace-1".into(),
            tab_id: "tab-1".into(),
            pane_id: pane_id.into(),
            focused: false,
            launch_pending: false,
            interactive_ready: true,
            state_change_seq,
            cwd: None,
            foreground_cwd: None,
            revision: 0,
        }
    }

    fn snapshot(agents: Vec<AgentInfo>) -> SessionSnapshot {
        SessionSnapshot {
            version: "0.8.0".into(),
            protocol: 20,
            session_epoch: "session".into(),
            event_cursor: herdr_client::EventCursor {
                stream_id: "stream".into(),
                sequence: 42,
            },
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            layouts: Vec::new(),
            agents,
        }
    }

    fn snapshot_with_tab(info: AgentInfo, label: &str, number: usize) -> SessionSnapshot {
        let mut snapshot = snapshot(vec![info]);
        snapshot.workspaces.push(WorkspaceInfo {
            workspace_id: "workspace-1".into(),
            number: 1,
            label: "project".into(),
            focused: false,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: "tab-1".into(),
            agent_status: "idle".into(),
            tokens: HashMap::new(),
            worktree: None,
        });
        snapshot.tabs.push(TabInfo {
            tab_id: "tab-1".into(),
            workspace_id: "workspace-1".into(),
            number,
            label: label.into(),
            focused: false,
            pane_count: 1,
            agent_status: "idle".into(),
        });
        snapshot
    }

    #[test]
    fn agents_prioritize_attention_and_recency_and_focus_panes() {
        let snapshot = snapshot(vec![
            agent("idle", "idle", 1),
            agent("blocked-old", "blocked", 2),
            agent("unknown", "future", 99),
            agent("working", "working", 4),
            agent("done", "done", 3),
            agent("blocked-new", "blocked", 5),
        ]);

        let items = agents(&snapshot);
        assert_eq!(
            items
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            [
                "workspace-1: blocked-new",
                "workspace-1: blocked-old",
                "workspace-1: done",
                "workspace-1: working",
                "workspace-1: idle",
                "workspace-1: unknown"
            ]
        );
        assert_eq!(items[0].dispatch.params, json!({ "target": "blocked-new" }));
    }

    #[test]
    fn agent_titles_prefix_the_workspace_and_prefer_name_then_tab_then_terminal_then_id() {
        for (name, tab, stripped, raw, terminal, expected) in [
            (
                Some("reviewer"),
                "agents",
                Some("Fix login"),
                None,
                "terminal-pane-1",
                "project: reviewer",
            ),
            (
                None,
                "agents",
                Some("Fix login"),
                None,
                "terminal-pane-1",
                "project: agents",
            ),
            (
                None,
                "1",
                Some("Fix login"),
                None,
                "terminal-pane-1",
                "project: Fix login",
            ),
            (
                None,
                "   ",
                Some("Fix login"),
                None,
                "terminal-pane-1",
                "project: Fix login",
            ),
            (
                None,
                "1",
                None,
                Some("Raw terminal title"),
                "terminal-pane-1",
                "project: Raw terminal title",
            ),
            (
                None,
                "1",
                None,
                None,
                "terminal-pane-1",
                "project: terminal-pane-1",
            ),
            (
                Some("project"),
                "project",
                Some("project"),
                Some("Fix login"),
                "terminal-pane-1",
                "project: Fix login",
            ),
            (
                Some("project"),
                "project",
                Some("project"),
                Some("project"),
                "terminal-pane-1",
                "project: terminal-pane-1",
            ),
            (
                Some("project"),
                "project",
                Some("project"),
                Some("project"),
                "project",
                "project",
            ),
        ] {
            let mut info = agent("pane-1", "idle", 1);
            info.name = name.map(str::to_owned);
            info.terminal_title_stripped = stripped.map(str::to_owned);
            info.terminal_title = raw.map(str::to_owned);
            info.terminal_id = terminal.into();
            assert_eq!(agents(&snapshot_with_tab(info, tab, 99))[0].title, expected);
        }
    }

    #[test]
    fn agents_put_path_and_kind_in_subtitle() {
        let mut info = agent("pane-1", "blocked", 1);
        info.name = None;
        info.agent = Some("codex".into());
        info.title = Some("Review the diff".into());
        info.cwd = Some("/project".into());
        info.foreground_cwd = Some("/foreground-project".into());
        let item = agents(&snapshot_with_tab(info, "agents", 1)).pop().unwrap();

        assert_eq!(item.title, "project: agents");
        assert_eq!(item.subtitle, "/foreground-project · codex");
        assert_eq!(item.agent_status, Some("blocked".into()));
        assert!(item.detail.contains("Review the diff"));

        for (cwd, foreground, kind, expected) in [
            (
                Some("/project"),
                Some("  "),
                Some("codex"),
                "/project · codex",
            ),
            (None, None, Some("codex"), "codex"),
            (Some("/project"), None, None, "/project"),
            (None, None, None, ""),
        ] {
            let mut info = agent("pane-1", "blocked", 1);
            info.name = None;
            info.agent = kind.map(str::to_owned);
            info.cwd = cwd.map(str::to_owned);
            info.foreground_cwd = foreground.map(str::to_owned);
            assert_eq!(
                agents(&snapshot_with_tab(info, "agents", 1))[0].subtitle,
                expected
            );
        }
    }
}
