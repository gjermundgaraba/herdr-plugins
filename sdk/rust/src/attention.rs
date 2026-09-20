//! Shared agent attention ordering: which agent most deserves a glance.
use std::cmp::Ordering;

use crate::AgentStatus;

/// Anything with an agent status, a lifecycle sequence, and a stable id.
pub trait Attention {
    fn agent_status(&self) -> &AgentStatus;
    fn state_change_seq(&self) -> u64;
    fn id(&self) -> &str;
}

impl Attention for crate::AgentInfo {
    fn agent_status(&self) -> &AgentStatus {
        &self.agent_status
    }
    fn state_change_seq(&self) -> u64 {
        self.state_change_seq
    }
    fn id(&self) -> &str {
        &self.terminal_id
    }
}

#[cfg(unix)]
impl Attention for crate::frontend::Agent {
    fn agent_status(&self) -> &AgentStatus {
        &self.agent_status
    }
    fn state_change_seq(&self) -> u64 {
        self.state_change_seq
    }
    fn id(&self) -> &str {
        &self.pane_id
    }
}

pub fn attention_rank(status: &AgentStatus) -> u8 {
    match status.as_str() {
        AgentStatus::BLOCKED => 4,
        AgentStatus::DONE => 3,
        AgentStatus::WORKING => 2,
        AgentStatus::IDLE => 1,
        _ => 0,
    }
}

/// Most urgent first; sequence numbers only compare within one session.
pub fn attention_order<T: Attention>(a: (&str, &T), b: (&str, &T)) -> Ordering {
    let (a_session, a) = a;
    let (b_session, b) = b;
    attention_rank(b.agent_status())
        .cmp(&attention_rank(a.agent_status()))
        .then_with(|| a_session.cmp(b_session))
        .then_with(|| b.state_change_seq().cmp(&a.state_change_seq()))
        .then_with(|| a.id().cmp(b.id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe(&'static str, AgentStatus, u64);
    impl Attention for Probe {
        fn agent_status(&self) -> &AgentStatus {
            &self.1
        }
        fn state_change_seq(&self) -> u64 {
            self.2
        }
        fn id(&self) -> &str {
            self.0
        }
    }
    fn agent(id: &'static str, status: &str, seq: u64) -> Probe {
        Probe(id, status.into(), seq)
    }

    #[test]
    fn attention_order_uses_status_session_sequence_then_id() {
        let mut agents = [
            agent("idle", "idle", 9),
            agent("working", "working", 8),
            agent("blocked-b", "blocked", 7),
            agent("done", "done", 6),
            agent("blocked-a", "blocked", 7),
            agent("blocked-new", "blocked", 10),
            agent("unknown", "future-status", 20),
        ];
        agents.sort_by(|a, b| attention_order(("local", a), ("local", b)));
        assert_eq!(
            agents.iter().map(|agent| agent.0).collect::<Vec<_>>(),
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
    fn sequence_never_compares_across_sessions_and_status_stays_global() {
        let old_local = agent("same", "blocked", 1);
        let new_remote = agent("same", "blocked", 10_000);
        assert!(attention_order(("local", &old_local), ("studio", &new_remote)).is_lt());
        assert!(attention_order(("studio", &new_remote), ("local", &old_local)).is_gt());
        let blocked = agent("blocked", "blocked", 0);
        let done = agent("done", "done", u64::MAX);
        assert!(attention_order(("z", &blocked), ("a", &done)).is_lt());
    }
}
