//! Live agent source: streams attention-ordered agent items to herdr-picker.

use std::{cmp::Reverse, collections::HashMap, process::ExitCode};

use herdr_client::{AgentStatus, SessionSnapshot};
use herdr_picker_sdk::{Item, presentation, run, serve};
use serde_json::json;

fn main() -> ExitCode {
    run(env!("CARGO_BIN_NAME"), serve(agent_items))
}

fn agent_items(snapshot: &SessionSnapshot) -> Vec<Item> {
    let workspace_labels = snapshot
        .workspaces
        .iter()
        .map(|workspace| (workspace.workspace_id.as_str(), workspace.label.as_str()))
        .collect::<HashMap<_, _>>();
    let tab_labels = snapshot
        .tabs
        .iter()
        .map(|tab| (tab.tab_id.as_str(), tab.label.as_str()))
        .collect::<HashMap<_, _>>();
    let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
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
            let cwd = nonempty(agent.foreground_cwd.as_deref())
                .or_else(|| nonempty(agent.cwd.as_deref()));
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
                id: agent.pane_id.clone(),
                title,
                subtitle,
                detail,
                badge: kind.unwrap_or_default().into(),
                indicator: indicator.into(),
                tone: Some(tone),
                spinning,
                search: String::new(),
                value: json!({
                    "pane_id": agent.pane_id,
                    "workspace_id": agent.workspace_id,
                    "tab_id": agent.tab_id,
                    "status": agent.agent_status,
                    "name": agent.name,
                    "agent": agent.agent,
                }),
            }
        })
        .collect()
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

    fn snapshot(agents: Vec<AgentInfo>) -> SessionSnapshot {
        SessionSnapshot {
            version: "0.8.0".into(),
            protocol: 19,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
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
            panes: Vec::new(),
            layouts: Vec::new(),
            agents,
        }
    }

    #[test]
    fn agent_picker_prioritizes_attention_and_uses_generic_presentation() {
        let items = agent_items(&snapshot(vec![
            agent("idle", "idle", 1),
            agent("blocked-old", "blocked", 2),
            agent("done", "done", 3),
            agent("working", "working", 4),
            agent("blocked-new", "blocked", 5),
        ]));
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["blocked-new", "blocked-old", "done", "working", "idle"]
        );
        assert_eq!(items[0].tone, Some(herdr_picker_sdk::Tone::Danger));
        assert!(items[3].spinning);
        assert_eq!(items[0].value["pane_id"], "blocked-new");
    }

    #[test]
    fn tab_label_is_not_used_as_the_agent_title() {
        let mut agent = agent("pane", "idle", 1);
        agent.name = None;
        agent.terminal_title_stripped = Some("shell".into());

        let item = agent_items(&snapshot(vec![agent])).remove(0);

        assert_eq!(item.title, "project: shell");
        assert!(item.detail.contains("agents"));
    }
}
