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

pub fn attention_order(a: (&str, &AgentInfo), b: (&str, &AgentInfo)) -> Ordering {
    let (a_session, a) = a;
    let (b_session, b) = b;
    attention_rank(&b.agent_status)
        .cmp(&attention_rank(&a.agent_status))
        .then_with(|| a_session.cmp(b_session))
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
    fn attention_order_uses_status_session_sequence_then_terminal() {
        let mut agents = [
            agent("idle", "idle", 9),
            agent("working", "working", 8),
            agent("blocked-b", "blocked", 7),
            agent("done", "done", 6),
            agent("blocked-a", "blocked", 7),
            agent("blocked-new", "blocked", 10),
            agent("unknown", "future-status", 20),
        ];
        agents.sort_by(|a, b| attention_order(("local/default", a), ("local/default", b)));
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

    #[test]
    fn attention_order_does_not_compare_sequence_across_sessions() {
        let old_local = agent("same", "blocked", 1);
        let new_remote = agent("same", "blocked", 10_000);

        assert!(
            attention_order(
                ("local/default", &old_local),
                ("studio/default", &new_remote)
            )
            .is_lt()
        );
        assert!(
            attention_order(
                ("studio/default", &new_remote),
                ("local/default", &old_local)
            )
            .is_gt()
        );
    }

    #[test]
    fn attention_order_keeps_status_global() {
        let blocked = agent("blocked", "blocked", 0);
        let done = agent("done", "done", u64::MAX);

        assert!(attention_order(("z/session", &blocked), ("a/session", &done)).is_lt());
    }
}
