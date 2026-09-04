use std::cmp::Ordering;

use herdr_client::{AgentInfo, AgentStatus};

pub fn attention_rank(status: &AgentStatus) -> u8 {
    match status.as_str() {
        AgentStatus::BLOCKED => 4,
        AgentStatus::DONE => 3,
        AgentStatus::WORKING => 2,
        AgentStatus::IDLE => 1,
        _ => 0,
    }
}

pub fn attention_order(a: &AgentInfo, b: &AgentInfo) -> Ordering {
    attention_rank(&b.agent_status)
        .cmp(&attention_rank(&a.agent_status))
        .then_with(|| b.state_change_seq.cmp(&a.state_change_seq))
        .then_with(|| a.terminal_id.cmp(&b.terminal_id))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn agent(terminal_id: &str, status: &str, state_change_seq: u64) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.into(),
            name: None,
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
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            pane_id: format!("pane-{terminal_id}"),
            focused: false,
            launch_pending: false,
            interactive_ready: true,
            state_change_seq,
            cwd: None,
            foreground_cwd: None,
            revision: 0,
        }
    }

    #[test]
    fn attention_order_uses_status_sequence_then_terminal() {
        let mut agents = [
            agent("idle", "idle", 9),
            agent("working", "working", 8),
            agent("blocked-b", "blocked", 7),
            agent("done", "done", 6),
            agent("blocked-a", "blocked", 7),
            agent("blocked-new", "blocked", 10),
            agent("unknown", "future-status", 20),
        ];
        agents.sort_by(attention_order);
        assert_eq!(
            agents
                .iter()
                .map(|agent| agent.terminal_id.as_str())
                .collect::<Vec<_>>(),
            [
                "blocked-new",
                "blocked-a",
                "blocked-b",
                "done",
                "working",
                "idle",
                "unknown",
            ]
        );
    }
}
