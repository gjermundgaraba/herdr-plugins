//! Projection of frontend-socket agents into the socket API's agent records.
use herdr_client::AgentInfo;
use herdr_frontend as frontend;

pub fn project_agent(agent: &frontend::Agent, snapshot: &frontend::ShellSnapshot) -> AgentInfo {
    let pane = snapshot.panes.iter().find(|p| p.pane_id == agent.pane_id);
    AgentInfo {
        terminal_id: agent.pane_id.clone(),
        name: agent.name.clone(),
        agent: agent.agent.clone(),
        title: agent.title.clone(),
        terminal_title: agent.terminal_title.clone(),
        terminal_title_stripped: agent.terminal_title_stripped.clone(),
        display_agent: agent.display_agent.clone(),
        agent_status: agent.agent_status.as_str().into(),
        screen_detection_skipped: false,
        state_labels: agent.state_labels.iter().cloned().collect(),
        tokens: agent.tokens.iter().cloned().collect(),
        agent_session: None,
        workspace_id: agent.workspace_id.clone(),
        tab_id: agent.tab_id.clone(),
        pane_id: agent.pane_id.clone(),
        focused: agent.focused,
        launch_pending: false,
        interactive_ready: false,
        state_change_seq: agent.state_change_seq,
        cwd: pane.and_then(|p| p.cwd.clone()),
        foreground_cwd: pane.and_then(|p| p.foreground_cwd.clone()),
        revision: snapshot.revision,
    }
}
