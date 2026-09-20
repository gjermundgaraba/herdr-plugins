use std::collections::{HashMap, HashSet};
use std::path::Path;

use herdr_client::{AgentInfo, AgentStatus, WorkspaceInfo};
use std::cmp::Ordering;

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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SlotKey {
    pub session: String,
    pub terminal_id: String,
}

pub struct SlotAgent<'a> {
    pub key: SlotKey,
    pub agent: &'a AgentInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLabel {
    pub title: String,
    pub subtitle: String,
}

pub fn assign_slots(
    previous: &[Option<SlotKey>],
    agents: &[SlotAgent<'_>],
    count: usize,
) -> Vec<Option<SlotKey>> {
    let mut sorted = agents.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| {
        attention_order(
            (a.key.session.as_str(), a.agent),
            (b.key.session.as_str(), b.agent),
        )
    });
    let by_id = agents
        .iter()
        .map(|agent| (agent.key.clone(), agent.agent))
        .collect::<HashMap<_, _>>();
    let mut slots = (0..count)
        .map(|index| {
            previous
                .get(index)
                .and_then(Option::as_ref)
                .filter(|id| by_id.contains_key(*id))
                .cloned()
        })
        .collect::<Vec<_>>();
    let mut slotted = slots.iter().flatten().cloned().collect::<HashSet<_>>();

    for candidate in sorted {
        if slotted.contains(&candidate.key) {
            continue;
        }
        if let Some(empty) = slots.iter().position(Option::is_none) {
            slots[empty] = Some(candidate.key.clone());
            slotted.insert(candidate.key.clone());
            continue;
        }
        if slots.is_empty() {
            break;
        }
        let victim = (1..slots.len()).fold(0, |victim, index| {
            let current_key = slots[index].as_ref().unwrap();
            let weakest_key = slots[victim].as_ref().unwrap();
            let current = by_id[current_key];
            let weakest = by_id[weakest_key];
            if attention_order(
                (current_key.session.as_str(), current),
                (weakest_key.session.as_str(), weakest),
            )
            .is_gt()
            {
                index
            } else {
                victim
            }
        });
        let displaced_key = slots[victim].as_ref().unwrap();
        let displaced = by_id[displaced_key];
        if attention_rank(&candidate.agent.agent_status) <= attention_rank(&displaced.agent_status)
        {
            break;
        }
        slotted.remove(displaced_key);
        slots[victim] = Some(candidate.key.clone());
        slotted.insert(candidate.key.clone());
    }
    slots
}

pub fn resize_slots(previous: &[Option<SlotKey>], count: usize) -> Vec<Option<SlotKey>> {
    (0..count)
        .map(|index| previous.get(index).cloned().flatten())
        .collect()
}

pub fn agent_label(agent: &AgentInfo, workspace: Option<&WorkspaceInfo>) -> AgentLabel {
    let title = agent
        .name
        .as_ref()
        .or(agent.display_agent.as_ref())
        .or(agent.agent.as_ref())
        .cloned()
        .unwrap_or_else(|| "agent".into());
    let repo = agent
        .foreground_cwd
        .as_ref()
        .or(agent.cwd.as_ref())
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned());
    let subtitle = workspace
        .map(|workspace| workspace.label.clone())
        .or(repo)
        .or_else(|| agent.terminal_title_stripped.clone())
        .or_else(|| agent.terminal_title.clone())
        .unwrap_or_default();
    AgentLabel { title, subtitle }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn agent(id: &str, status: &str, seq: u64) -> AgentInfo {
        AgentInfo {
            terminal_id: id.into(),
            name: None,
            agent: Some(id.into()),
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
            pane_id: format!("pane-{id}"),
            focused: false,
            launch_pending: false,
            interactive_ready: true,
            state_change_seq: seq,
            cwd: None,
            foreground_cwd: None,
            revision: 0,
        }
    }

    fn candidates<'a>(session: &str, agents: &'a [AgentInfo]) -> Vec<SlotAgent<'a>> {
        agents
            .iter()
            .map(|agent| SlotAgent {
                key: SlotKey {
                    session: session.into(),
                    terminal_id: agent.terminal_id.clone(),
                },
                agent,
            })
            .collect()
    }

    #[test]
    fn slot_assignment_is_sticky_prioritized_and_session_scoped() {
        let agents = vec![
            agent("idle", "idle", 5),
            agent("blocked", "blocked", 1),
            agent("working", "working", 9),
        ];
        let default_candidates = candidates("local/default", &agents);
        let assigned = assign_slots(&[], &default_candidates, 2);
        assert_eq!(assigned[0].as_ref().unwrap().terminal_id, "blocked");
        assert_eq!(assigned[1].as_ref().unwrap().terminal_id, "working");
        assert_eq!(assign_slots(&assigned, &default_candidates, 2), assigned);

        let duplicate = vec![agent("blocked", "done", 2)];
        let mut both = default_candidates;
        both.extend(candidates("local/review", &duplicate));
        let assigned = assign_slots(&[], &both, 3);
        assert!(assigned.contains(&Some(SlotKey {
            session: "local/default".into(),
            terminal_id: "blocked".into(),
        })));
        assert!(assigned.contains(&Some(SlotKey {
            session: "local/review".into(),
            terminal_id: "blocked".into(),
        })));

        assert_eq!(
            resize_slots(&assigned, 4),
            [
                assigned[0].clone(),
                assigned[1].clone(),
                assigned[2].clone(),
                None
            ]
        );
    }

    #[test]
    fn cross_session_assignment_orders_by_session_before_sequence() {
        let older = vec![agent("older", "blocked", 1)];
        let newer = vec![agent("newer", "blocked", 10_000)];
        let mut both = candidates("studio/default", &newer);
        both.extend(candidates("local/default", &older));

        let assigned = assign_slots(&[], &both, 2);

        assert_eq!(assigned[0].as_ref().unwrap().session, "local/default");
        assert_eq!(assigned[1].as_ref().unwrap().session, "studio/default");
    }

    #[test]
    fn labels_follow_name_and_location_fallbacks() {
        let mut value = agent("pi", "idle", 1);
        value.name = Some("builder".into());
        value.cwd = Some("/tmp/repo".into());
        let workspace = WorkspaceInfo {
            workspace_id: "w1".into(),
            number: 1,
            label: "Project".into(),
            focused: true,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: "t1".into(),
            agent_status: "idle".into(),
            tokens: HashMap::new(),
            worktree: None,
        };
        assert_eq!(
            agent_label(&value, Some(&workspace)),
            AgentLabel {
                title: "builder".into(),
                subtitle: "Project".into(),
            }
        );

        value.name = None;
        value.agent = Some("claude".into());
        value.foreground_cwd = Some("/Users/gg/ws/pers/herdr-deck".into());
        assert_eq!(agent_label(&value, None).subtitle, "herdr-deck");
    }
}
