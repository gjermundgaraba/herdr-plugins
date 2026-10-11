//! Sticky slots for a device with a fixed number of agent keys.
//!
//! The daemon owns the order agents deserve attention in and sends it as
//! `priority_key`; free slots fill in that order. What a device adds is
//! stickiness: an agent keeps its key while it is in the report, so a key
//! does not change meaning under a finger when the order changes. That
//! holds while the agent cannot be reached, as when its endpoint
//! reconnects: the device shows such a key as unavailable, and it is held
//! like any other. A free key goes only to a newcomer that can be reached.
//!
//! Taking a full slot is the one exception, and the one place with a policy
//! of its own. It is deliberately the device's: a newcomer must both come
//! before the slotted agent in the daemon's order and have a louder status,
//! ranked here as blocked, done, working, idle. That ranking ignores the
//! daemon's priority lift, so a lifted `done` agent never displaces a
//! slotted `blocked` one although the daemon orders it first. Whether the
//! slotted agent can be reached plays no part: a key held by an agent that
//! cannot be reached is taken on the same terms.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

use crate::{AgentStatus, ClientReportAgent};

/// An agent pane, qualified by its endpoint so ids never collide across
/// machines.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SlotKey {
    pub endpoint_id: String,
    pub pane_id: String,
}

impl SlotKey {
    pub fn of(row: &ClientReportAgent) -> Self {
        Self {
            endpoint_id: row.endpoint_id.clone(),
            pane_id: row.agent.pane_id.clone(),
        }
    }

    pub fn is(&self, row: &ClientReportAgent) -> bool {
        self.endpoint_id == row.endpoint_id && self.pane_id == row.agent.pane_id
    }
}

/// A row with its place in the report.
pub type Placed<'a> = (usize, &'a ClientReportAgent);

/// The daemon's priority order: `priority_key` compared as plain bytes,
/// smallest first. Equal keys are legal and keep the report's own order, as
/// do agents of a daemon that sends no key, which follow all keyed ones.
pub fn priority_order(a: Placed<'_>, b: Placed<'_>) -> Ordering {
    fn key<'a>((index, row): Placed<'a>) -> (bool, &'a [u8], usize) {
        let key = row.agent.priority_key.as_bytes();
        (key.is_empty(), key, index)
    }
    key(a).cmp(&key(b))
}

/// The row that is first in the daemon's priority order.
pub fn first_in_priority<'a>(
    rows: impl IntoIterator<Item = &'a ClientReportAgent>,
) -> Option<&'a ClientReportAgent> {
    rows.into_iter()
        .enumerate()
        .min_by(|a, b| priority_order(*a, *b))
        .map(|(_, row)| row)
}

/// How loudly a status asks for a slot. This is a stickiness threshold, not
/// an order: an agent that the daemon ranks ahead of a slotted one still
/// only takes its slot when it is also in a louder state.
fn slot_claim(row: &ClientReportAgent) -> u8 {
    match row.agent.agent_status.as_str() {
        AgentStatus::BLOCKED => 4,
        AgentStatus::DONE => 3,
        AgentStatus::WORKING => 2,
        AgentStatus::IDLE => 1,
        _ => 0,
    }
}

/// Assigns `count` sticky slots. An agent keeps its slot while it is in
/// `rows`, whether `reachable` or not. Free slots go to the unslotted agents
/// that are `reachable`, in the daemon's priority order. With every slot
/// taken, such an agent replaces the slotted agent that is last in that
/// order, but only when it comes before that agent in the order and has a
/// louder status. A change of recency alone therefore never moves a key.
///
/// `rows` is every agent the client reports that the device shows.
/// `reachable` says whether a jump can show a row now; one that is not
/// takes no slot it does not hold.
pub fn assign_slots<'a>(
    previous: &[Option<SlotKey>],
    rows: impl IntoIterator<Item = &'a ClientReportAgent>,
    count: usize,
    reachable: impl Fn(&ClientReportAgent) -> bool,
) -> Vec<Option<SlotKey>> {
    let mut ordered: Vec<Placed<'a>> = rows.into_iter().enumerate().collect();
    let by_key: HashMap<SlotKey, Placed<'a>> = ordered
        .iter()
        .map(|placed| (SlotKey::of(placed.1), *placed))
        .collect();
    ordered.sort_by(|a, b| priority_order(*a, *b));
    let mut slots: Vec<_> = (0..count)
        .map(|index| {
            previous
                .get(index)
                .and_then(Option::as_ref)
                .filter(|key| by_key.contains_key(*key))
                .cloned()
        })
        .collect();
    let mut slotted: HashSet<_> = slots.iter().flatten().cloned().collect();
    for placed in ordered {
        let candidate = placed.1;
        let key = SlotKey::of(candidate);
        if slotted.contains(&key) || !reachable(candidate) {
            continue;
        }
        if let Some(empty) = slots.iter().position(Option::is_none) {
            slots[empty] = Some(key.clone());
            slotted.insert(key);
            continue;
        }
        // Every slot is taken, by agents that are all in the report.
        let Some((victim, displaced)) = slots
            .iter()
            .enumerate()
            .filter_map(|(slot, key)| Some((slot, *by_key.get(key.as_ref()?)?)))
            .max_by(|a, b| priority_order(a.1, b.1))
        else {
            break;
        };
        // Candidates come in priority order, so once one sorts after every
        // slotted agent, so do all that follow.
        if priority_order(placed, displaced).is_ge() {
            break;
        }
        if slot_claim(candidate) <= slot_claim(displaced.1) {
            continue;
        }
        slotted.remove(&SlotKey::of(displaced.1));
        slots[victim] = Some(key.clone());
        slotted.insert(key);
    }
    slots
}

/// [`assign_slots`] for the followed client `client`. Slots outlive a time
/// when no client is followed and a device shows nothing, so the keys of a
/// client that is followed again mean the same: call this only while a
/// client is followed. Another client than `owner` starts with empty slots.
pub fn sticky_slots<'a, C: PartialEq>(
    slots: &[Option<SlotKey>],
    owner: &mut Option<C>,
    client: C,
    rows: impl IntoIterator<Item = &'a ClientReportAgent>,
    count: usize,
    reachable: impl Fn(&ClientReportAgent) -> bool,
) -> Vec<Option<SlotKey>> {
    let kept = if owner.as_ref() == Some(&client) {
        slots
    } else {
        &[]
    };
    *owner = Some(client);
    assign_slots(kept, rows, count, reachable)
}

/// Keeps the first `count` slots, adding empty ones when `count` grew.
pub fn resize_slots(previous: &[Option<SlotKey>], count: usize) -> Vec<Option<SlotKey>> {
    (0..count)
        .map(|index| previous.get(index).cloned().flatten())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClientAgent;

    fn row(endpoint: &str, pane: &str, status: &str, priority_key: &str) -> ClientReportAgent {
        ClientReportAgent {
            endpoint_id: endpoint.into(),
            focused: false,
            workspace_label: "api".into(),
            workspace_number: 1,
            cwd: None,
            foreground_cwd: None,
            agent: ClientAgent {
                pane_id: pane.into(),
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
                name: None,
                display_agent: None,
                agent: Some("claude".into()),
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: status.into(),
                state_change_seq: 1,
                state_labels: Vec::new(),
                tokens: Vec::new(),
                focused: false,
                priority_key: priority_key.into(),
                priority_mark: None,
            },
        }
    }

    /// Every agent can be reached.
    fn all(_: &ClientReportAgent) -> bool {
        true
    }

    fn panes(slots: &[Option<SlotKey>]) -> Vec<Option<&str>> {
        slots
            .iter()
            .map(|slot| slot.as_ref().map(|key| key.pane_id.as_str()))
            .collect()
    }

    #[test]
    fn priority_keys_order_new_slots_and_the_report_breaks_ties() {
        // Display order differs from priority order: a client sorted by space.
        let rows = [
            row("local", "idle", "idle", "1.3.fff0"),
            row("local", "working", "working", "1.2.fff0"),
            row("local", "tie-b", "done", "1.1.0000"),
            row("local", "tie-a", "done", "1.1.0000"),
            row("local", "keyless", "blocked", ""),
            row("local", "blocked", "blocked", "1.0.0001"),
        ];
        assert_eq!(
            panes(&assign_slots(&[], &rows, 6, all)),
            [
                Some("blocked"),
                Some("tie-b"),
                Some("tie-a"),
                Some("working"),
                Some("idle"),
                Some("keyless")
            ]
        );
        assert_eq!(
            first_in_priority(&rows).map(|row| row.agent.pane_id.as_str()),
            Some("blocked")
        );
        assert!(first_in_priority(&[]).is_none());
        // Fewer slots than agents: the first in priority order get them.
        assert_eq!(
            panes(&assign_slots(&[], &rows, 2, all)),
            [Some("blocked"), Some("tie-b")]
        );
        assert!(assign_slots(&[], &rows, 0, all).is_empty());
    }

    #[test]
    fn slots_are_sticky_and_only_a_louder_agent_takes_a_full_one() {
        let mut rows: Vec<_> = (0..6)
            .map(|n| {
                row(
                    "local",
                    &format!("idle-{n}"),
                    "idle",
                    &format!("1.3.000{n}"),
                )
            })
            .collect();
        let slots = assign_slots(&[], &rows, 6, all);
        assert_eq!(panes(&slots)[0], Some("idle-0"));

        // Status, title, and order changes never move a slotted agent.
        rows[5].agent.agent_status = "blocked".into();
        rows[5].agent.priority_key = "1.0.0000".into();
        rows[2].agent.title = Some("renamed".into());
        rows.reverse();
        assert_eq!(assign_slots(&slots, &rows, 6, all), slots);

        // A newcomer that is no louder than the last slotted agent waits,
        // even though its key sorts first among the idle agents.
        rows.push(row("local", "late-idle", "idle", "1.3.0000"));
        assert_eq!(assign_slots(&slots, &rows, 6, all), slots);

        // A louder one replaces the agent that is last in priority order.
        rows.push(row("local", "late-working", "working", "1.2.0000"));
        let next = assign_slots(&slots, &rows, 6, all);
        assert_eq!(panes(&next)[4], Some("late-working"));
        assert_eq!(
            next.iter()
                .zip(&slots)
                .filter(|(next, previous)| next != previous)
                .count(),
            1
        );

        // An agent that left the report frees its slot for the one waiting.
        rows.retain(|row| row.agent.pane_id != "idle-0");
        let next = assign_slots(&next, &rows, 6, all);
        assert_eq!(panes(&next)[0], Some("late-idle"));
        assert_eq!(panes(&next)[4], Some("late-working"));
    }

    #[test]
    fn a_louder_agent_the_daemon_ranks_later_does_not_take_a_slot() {
        // The daemon lifts the done agents of a priority space ahead of every
        // other agent, a blocked one included. A full board of them stays.
        let mut rows = vec![
            row("local", "lifted-a", "done", "0.1.0001"),
            row("local", "lifted-b", "done", "0.1.0002"),
        ];
        let slots = assign_slots(&[], &rows, 2, all);
        rows.insert(0, row("local", "blocked", "blocked", "1.0.0001"));
        assert_eq!(assign_slots(&slots, &rows, 2, all), slots);
        assert_eq!(
            panes(&assign_slots(&[], &rows, 2, all)),
            [Some("lifted-a"), Some("lifted-b")]
        );

        // Ranked ahead and louder: it takes the last one's slot.
        rows.push(row("local", "lifted-blocked", "blocked", "0.0.0001"));
        assert_eq!(
            panes(&assign_slots(&slots, &rows, 2, all)),
            [Some("lifted-a"), Some("lifted-blocked")]
        );
    }

    #[test]
    fn slots_wait_for_their_client_and_another_client_starts_empty() {
        let mut rows = vec![
            row("local", "a", "idle", "1.3.0001"),
            row("local", "b", "idle", "1.3.0002"),
            row("local", "c", "idle", "1.3.0003"),
        ];
        let mut owner = None;
        let slots = sticky_slots(&[], &mut owner, 7, &rows, 2, all);
        assert_eq!(panes(&slots), [Some("a"), Some("b")]);

        // The same client, back with its agents in another priority order.
        rows[2].agent.priority_key = "1.3.0000".into();
        assert_eq!(sticky_slots(&slots, &mut owner, 7, &rows, 2, all), slots);

        // Another client's agents are not this one's: nothing is kept.
        assert_eq!(
            panes(&sticky_slots(&slots, &mut owner, 8, &rows, 2, all)),
            [Some("c"), Some("a")]
        );
        assert_eq!(owner, Some(8));
    }

    #[test]
    fn an_agent_that_cannot_be_reached_keeps_its_slot_and_takes_none() {
        let mut rows = vec![
            row("local", "a", "idle", "1.3.0001"),
            row("ssh:build", "b", "working", "1.2.0001"),
            row("ssh:build", "c", "idle", "1.3.0002"),
            row("local", "d", "idle", "1.3.0003"),
        ];
        let slots = assign_slots(&[], &rows, 4, all);
        assert_eq!(panes(&slots), [Some("b"), Some("a"), Some("c"), Some("d")]);

        // The endpoint reconnects. Its agents are still in the report, so
        // their keys are neither freed nor refilled, though one waits.
        let local = |row: &ClientReportAgent| row.endpoint_id == "local";
        rows.push(row("local", "waiting", "idle", "1.3.0000"));
        assert_eq!(assign_slots(&slots, &rows, 4, local), slots);
        // A louder agent takes the slot of the last in priority order, as
        // it would with every endpoint online.
        rows.push(row("local", "loud", "blocked", "1.0.0000"));
        assert_eq!(
            panes(&assign_slots(&slots, &rows, 4, local)),
            [Some("b"), Some("a"), Some("c"), Some("loud")]
        );
        rows.pop();
        // The endpoint is back: every key means what it did.
        assert_eq!(assign_slots(&slots, &rows, 4, all), slots);

        // An agent that left the report while unreachable frees its key,
        // for an agent that can be reached only.
        rows.retain(|row| row.agent.pane_id != "c");
        rows.push(row("ssh:build", "newcomer", "blocked", "1.0.0000"));
        let next = assign_slots(&slots, &rows, 4, local);
        assert_eq!(
            panes(&next),
            [Some("b"), Some("a"), Some("waiting"), Some("d")]
        );
        // With no key held, one that cannot be reached gets none, and gets
        // one once it can.
        assert_eq!(
            panes(&assign_slots(&[], &rows, 6, local)),
            [Some("waiting"), Some("a"), Some("d"), None, None, None]
        );
        assert_eq!(
            panes(&assign_slots(&next, &rows, 4, all)),
            [Some("b"), Some("a"), Some("waiting"), Some("newcomer")]
        );
    }

    #[test]
    fn a_louder_agent_takes_a_full_slot_held_by_one_that_cannot_be_reached() {
        // Every key is held by an agent of an endpoint that reconnects.
        let mut rows = vec![
            row("ssh:build", "a", "idle", "1.3.0001"),
            row("ssh:build", "b", "idle", "1.3.0002"),
        ];
        let slots = assign_slots(&[], &rows, 2, all);
        let local = |row: &ClientReportAgent| row.endpoint_id == "local";
        assert_eq!(assign_slots(&slots, &rows, 2, local), slots);

        // Reachable newcomers the daemon ranks first: one that is no louder
        // waits, as it would for agents that can be reached.
        rows.push(row("local", "quiet", "idle", "1.3.0000"));
        assert_eq!(assign_slots(&slots, &rows, 2, local), slots);
        // A louder one takes the key of the last in priority order, though
        // that agent is still in the report.
        rows.push(row("local", "loud", "blocked", "1.0.0000"));
        assert_eq!(
            panes(&assign_slots(&slots, &rows, 2, local)),
            [Some("a"), Some("loud")]
        );
        // A louder one that cannot be reached itself takes nothing.
        rows.pop();
        rows.push(row("ssh:build", "loud", "blocked", "1.0.0000"));
        assert_eq!(assign_slots(&slots, &rows, 2, local), slots);
    }

    #[test]
    fn the_same_pane_id_on_two_endpoints_is_two_agents() {
        let rows = [
            row("local", "w1:p1", "idle", "1.3.0001"),
            row("ssh:build", "w1:p1", "blocked", "1.0.0001"),
        ];
        let slots = assign_slots(&[], &rows, 3, all);
        assert_eq!(
            slots,
            [
                Some(SlotKey {
                    endpoint_id: "ssh:build".into(),
                    pane_id: "w1:p1".into()
                }),
                Some(SlotKey {
                    endpoint_id: "local".into(),
                    pane_id: "w1:p1".into()
                }),
                None
            ]
        );
        assert!(slots[0].as_ref().unwrap().is(&rows[1]));
        assert!(!slots[0].as_ref().unwrap().is(&rows[0]));

        assert_eq!(
            resize_slots(&slots, 4),
            [slots[0].clone(), slots[1].clone(), None, None]
        );
        assert_eq!(resize_slots(&slots, 1), [slots[0].clone()]);
    }
}
