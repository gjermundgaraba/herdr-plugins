use crate::config::{AgentStatus, Direction, Light, LightingConfig};
use codex_micro::service::Lighting;
use herdr_client::attention::{attention_order, attention_rank};
use herdr_client::frontend::Agent;
use std::collections::{HashMap, HashSet};

pub const SLOT_COUNT: usize = 6;

/// An agent pane, qualified by its endpoint so ids never collide across machines.
pub type SlotKey = (String, String);

pub fn slot_order(a: &(SlotKey, Agent), b: &(SlotKey, Agent)) -> std::cmp::Ordering {
    attention_order((&a.0.0, &a.1), (&b.0.0, &b.1))
}

#[derive(Clone, Debug, PartialEq)]
pub struct JoystickEvent {
    pub sector: Option<u8>,
    pub direction: Option<Direction>,
}

fn status(agent: &Agent) -> AgentStatus {
    match agent.agent_status.as_str() {
        "idle" => AgentStatus::Idle,
        "working" => AgentStatus::Working,
        "blocked" => AgentStatus::Blocked,
        "done" => AgentStatus::Done,
        _ => AgentStatus::Unknown,
    }
}
pub fn assign_slots(
    previous: &[Option<SlotKey>],
    agents: &[(SlotKey, Agent)],
) -> Vec<Option<SlotKey>> {
    let mut sorted: Vec<_> = agents.iter().collect();
    sorted.sort_by(|a, b| slot_order(a, b));
    let by_id: HashMap<_, _> = agents
        .iter()
        .map(|(key, _)| (key, agents_entry(agents, key)))
        .collect();
    let mut slots: Vec<_> = (0..SLOT_COUNT)
        .map(|index| {
            previous
                .get(index)
                .and_then(Option::as_ref)
                .filter(|key| by_id.contains_key(key))
                .cloned()
        })
        .collect();
    let mut slotted: HashSet<_> = slots.iter().flatten().cloned().collect();
    for candidate in sorted {
        if slotted.contains(&candidate.0) {
            continue;
        }
        if let Some(empty) = slots.iter().position(Option::is_none) {
            slots[empty] = Some(candidate.0.clone());
            slotted.insert(candidate.0.clone());
            continue;
        }
        let victim = (1..SLOT_COUNT).fold(0, |victim, index| {
            if slot_order(
                by_id[slots[index].as_ref().unwrap()],
                by_id[slots[victim].as_ref().unwrap()],
            )
            .is_gt()
            {
                index
            } else {
                victim
            }
        });
        let displaced = by_id[slots[victim].as_ref().unwrap()];
        if attention_rank(&candidate.1.agent_status) <= attention_rank(&displaced.1.agent_status) {
            break;
        }
        slotted.remove(&displaced.0);
        slots[victim] = Some(candidate.0.clone());
        slotted.insert(candidate.0.clone());
    }
    slots
}
fn agents_entry<'a>(agents: &'a [(SlotKey, Agent)], key: &SlotKey) -> &'a (SlotKey, Agent) {
    agents
        .iter()
        .find(|(candidate, _)| candidate == key)
        .expect("key taken from the same list")
}
fn slot_lighting(
    slots: &[Option<SlotKey>],
    by_id: &HashMap<&SlotKey, &Agent>,
    config: &LightingConfig,
) -> [Light; SLOT_COUNT] {
    std::array::from_fn(|index| {
        slots
            .get(index)
            .and_then(Option::as_ref)
            .and_then(|key| by_id.get(key))
            .map(|a| {
                let mut l = config.light(status(a));
                if a.focused {
                    l.b = l.b.max(config.focused_brightness);
                }
                l
            })
            .unwrap_or_default()
    })
}
pub fn lighting(
    slots: &[Option<SlotKey>],
    agents: &[(SlotKey, Agent)],
    config: &LightingConfig,
) -> Lighting {
    let by_id: HashMap<_, _> = agents.iter().map(|(key, agent)| (key, agent)).collect();
    let best = slots
        .iter()
        .flatten()
        .filter_map(|key| by_id.get(key).map(|agent| (key, *agent)))
        .min_by(|a, b| attention_order((&a.0.0, a.1), (&b.0.0, b.1)));
    let light = best
        .map(|(_, a)| config.light(status(a)))
        .unwrap_or_default();
    Lighting {
        ambient: if config.ambient {
            light
        } else {
            Light::default()
        },
        keys: if config.keys { light } else { Light::default() },
        slots: slot_lighting(slots, &by_id, config),
    }
}
pub fn joystick_event(
    angle: f64,
    distance: f64,
    last_sector: Option<u8>,
    engage_distance: f64,
    release_distance: f64,
) -> JoystickEvent {
    if !angle.is_finite() || !distance.is_finite() || distance <= release_distance {
        return JoystickEvent {
            sector: if distance <= release_distance {
                None
            } else {
                last_sector
            },
            direction: None,
        };
    }
    if last_sector.is_none() && distance < engage_distance {
        return JoystickEvent {
            sector: None,
            direction: None,
        };
    }
    let sector = ((angle * 4.0).round() as i64).rem_euclid(4) as u8;
    JoystickEvent {
        sector: Some(sector),
        direction: if Some(sector) == last_sector {
            None
        } else {
            Some(
                [
                    Direction::Right,
                    Direction::Down,
                    Direction::Left,
                    Direction::Up,
                ][sector as usize],
            )
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;
    fn agent(id: &str, status: &str, focused: bool) -> (SlotKey, Agent) {
        let agent = serde_json::from_value(json!({
            "agent_status":status, "workspace_id":"w1", "tab_id":"w1:t1", "pane_id":id,
            "focused":focused, "state_change_seq":1, "state_labels":[], "tokens":[]
        }))
        .unwrap();
        (("local".into(), id.into()), agent)
    }
    fn key(id: &str) -> Option<SlotKey> {
        Some(("local".into(), id.into()))
    }
    #[test]
    fn sticky_slots_lighting_and_joystick() {
        let agents = [
            agent("idle", "idle", true),
            agent("working", "working", false),
        ];
        let slots = assign_slots(&[], &agents);
        assert_eq!(slots[..2], [key("working"), key("idle")]);
        assert_eq!(
            joystick_event(0.25, 0.9, Some(0), 0.75, 0.3).direction,
            Some(Direction::Down)
        );
        assert_eq!(joystick_event(0.0, 0.1, Some(0), 0.75, 0.3).sector, None);
        let config = Config::default().lighting;
        let mut focused_idle = config.light(AgentStatus::Idle);
        focused_idle.b = focused_idle.b.max(config.focused_brightness);
        let empty = Light::default();
        assert_eq!(
            lighting(&slots, &agents, &config).slots.as_slice(),
            [config.light(AgentStatus::Working), focused_idle]
                .into_iter()
                .chain(std::iter::repeat_n(empty, SLOT_COUNT - 2))
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn disabling_aggregates_produces_explicit_off_in_complete_snapshot() {
        let agents = [agent("working", "working", false)];
        let slots = assign_slots(&[], &agents);
        let mut config = Config::default().lighting;
        config.ambient = true;
        config.keys = true;
        let lit = lighting(&slots, &agents, &config);
        assert_ne!(lit.ambient, Light::default());
        assert_ne!(lit.keys, Light::default());
        config.ambient = false;
        config.keys = false;
        let disabled = lighting(&slots, &agents, &config);
        assert_eq!(disabled.ambient, Light::default());
        assert_eq!(disabled.keys, Light::default());
        assert_eq!(disabled.slots, lit.slots);
        assert_eq!(lighting(&[], &agents, &config), Lighting::default());
    }

    #[test]
    fn joystick_changes_direction_at_an_angular_boundary() {
        let first = joystick_event(0.124, 0.9, None, 0.75, 0.3);
        assert_eq!(first.direction, Some(Direction::Right));
        let across = joystick_event(0.126, 0.9, first.sector, 0.75, 0.3);
        assert_eq!(across.direction, Some(Direction::Down));
        let back = joystick_event(0.124, 0.9, across.sector, 0.75, 0.3);
        assert_eq!(back.direction, Some(Direction::Right));
    }
}
