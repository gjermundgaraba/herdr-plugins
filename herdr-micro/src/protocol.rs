use crate::{
    actions::CHATGPT_BUNDLE_IDS,
    config::{AgentStatus, Light, LightingConfig},
};
use herdr_client::AgentInfo;
use std::collections::{HashMap, HashSet};

pub const SLOT_COUNT: usize = 6;
pub const INPUT_BUNDLE_ID: &str = "it.focusense.input-app";

#[derive(Clone, Debug, PartialEq)]
pub struct JoystickEvent {
    pub sector: Option<u8>,
    pub direction: Option<&'static str>,
}

pub fn device_owner(input_running: bool, frontmost_bundle: Option<&str>) -> Option<&'static str> {
    if input_running {
        return Some("Input");
    }
    frontmost_bundle
        .is_some_and(|bundle| CHATGPT_BUNDLE_IDS.contains(&bundle))
        .then_some("ChatGPT")
}

fn status(agent: &AgentInfo) -> AgentStatus {
    match agent.agent_status.as_str() {
        "idle" => AgentStatus::Idle,
        "working" => AgentStatus::Working,
        "blocked" => AgentStatus::Blocked,
        "done" => AgentStatus::Done,
        _ => AgentStatus::Unknown,
    }
}
fn priority(agent: &AgentInfo) -> u8 {
    match status(agent) {
        AgentStatus::Unknown => 0,
        AgentStatus::Idle => 1,
        AgentStatus::Working => 2,
        AgentStatus::Done => 3,
        AgentStatus::Blocked => 4,
    }
}
fn compare(a: &AgentInfo, b: &AgentInfo) -> std::cmp::Ordering {
    priority(b)
        .cmp(&priority(a))
        .then_with(|| b.state_change_seq.cmp(&a.state_change_seq))
}
pub fn assign_slots(previous: &[Option<String>], agents: &[AgentInfo]) -> Vec<Option<String>> {
    let mut sorted: Vec<_> = agents.iter().collect();
    sorted.sort_by(|a, b| compare(a, b));
    let by_id: HashMap<_, _> = agents
        .iter()
        .map(|agent| (agent.terminal_id.as_str(), agent))
        .collect();
    let mut slots: Vec<_> = (0..SLOT_COUNT)
        .map(|index| {
            previous
                .get(index)
                .and_then(Option::as_ref)
                .filter(|id| by_id.contains_key(id.as_str()))
                .cloned()
        })
        .collect();
    let mut slotted: HashSet<_> = slots.iter().flatten().cloned().collect();
    for candidate in sorted {
        if slotted.contains(&candidate.terminal_id) {
            continue;
        }
        if let Some(empty) = slots.iter().position(Option::is_none) {
            slots[empty] = Some(candidate.terminal_id.clone());
            slotted.insert(candidate.terminal_id.clone());
            continue;
        }
        let victim = (1..SLOT_COUNT).fold(0, |victim, index| {
            if compare(
                by_id[slots[index].as_ref().unwrap().as_str()],
                by_id[slots[victim].as_ref().unwrap().as_str()],
            ) == std::cmp::Ordering::Greater
            {
                index
            } else {
                victim
            }
        });
        let displaced = by_id[slots[victim].as_ref().unwrap().as_str()];
        if priority(candidate) <= priority(displaced) {
            break;
        }
        slotted.remove(&displaced.terminal_id);
        slots[victim] = Some(candidate.terminal_id.clone());
        slotted.insert(candidate.terminal_id.clone());
    }
    slots
}
pub fn slot_lighting(
    slots: &[Option<String>],
    agents: &[AgentInfo],
    config: &LightingConfig,
) -> Vec<Light> {
    let by_id: HashMap<_, _> = agents.iter().map(|a| (a.terminal_id.as_str(), a)).collect();
    slots
        .iter()
        .map(|slot| {
            slot.as_ref()
                .and_then(|id| by_id.get(id.as_str()))
                .map(|a| {
                    let mut l = config.light(status(a));
                    if a.focused {
                        l.b = l.b.max(config.focused_brightness);
                    }
                    l
                })
                .unwrap_or(Light {
                    c: 0,
                    b: 0.0,
                    e: 0,
                    s: 0.0,
                })
        })
        .collect()
}
pub fn aggregate_lighting(
    slots: &[Option<String>],
    agents: &[AgentInfo],
    config: &LightingConfig,
) -> HashMap<String, Light> {
    let by_id: HashMap<_, _> = agents.iter().map(|a| (a.terminal_id.as_str(), a)).collect();
    let best = slots
        .iter()
        .flatten()
        .filter_map(|id| by_id.get(id.as_str()))
        .min_by(|a, b| compare(a, b));
    let light = best.map(|a| config.light(status(a))).unwrap_or(Light {
        c: 0,
        b: 0.0,
        e: 0,
        s: 0.0,
    });
    ["ambient", "keys"]
        .into_iter()
        .filter(|zone| match *zone {
            "ambient" => config.ambient.as_deref() == Some("status"),
            _ => config.keys.as_deref() == Some("status"),
        })
        .map(|zone| (zone.to_owned(), light))
        .collect()
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
            Some(["right", "down", "left", "up"][sector as usize])
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;
    fn agent(id: &str, status: &str) -> AgentInfo {
        serde_json::from_value(json!({
            "terminal_id":id, "agent_status":status, "workspace_id":"w1",
            "tab_id":"w1:t1", "pane_id":"w1:p1", "focused":false, "revision":1
        }))
        .unwrap()
    }
    #[test]
    fn sticky_slots_lighting_and_joystick() {
        let slots = assign_slots(&[], &[agent("idle", "idle"), agent("working", "working")]);
        assert_eq!(slots[..2], [Some("working".into()), Some("idle".into())]);
        assert_eq!(
            joystick_event(0.25, 0.9, Some(0), 0.75, 0.3).direction,
            Some("down")
        );
        assert_eq!(joystick_event(0.0, 0.1, Some(0), 0.75, 0.3).sector, None);
        assert_eq!(
            slot_lighting(
                &slots,
                &[agent("idle", "idle"), agent("working", "working")],
                &Config::default().lighting
            )
            .len(),
            SLOT_COUNT
        );
    }
    #[test]
    fn device_owners_use_bundle_identity() {
        assert_eq!(device_owner(true, None), Some("Input"));

        assert_eq!(
            device_owner(false, Some("com.openai.codex")),
            Some("ChatGPT")
        );
        assert_eq!(
            device_owner(false, Some("com.openai.chat")),
            Some("ChatGPT")
        );
        assert_eq!(device_owner(false, Some("com.example.other")), None);
    }

    #[test]
    fn joystick_changes_direction_at_an_angular_boundary() {
        let first = joystick_event(0.124, 0.9, None, 0.75, 0.3);
        assert_eq!(first.direction, Some("right"));
        let across = joystick_event(0.126, 0.9, first.sector, 0.75, 0.3);
        assert_eq!(across.direction, Some("down"));
        let back = joystick_event(0.124, 0.9, across.sector, 0.75, 0.3);
        assert_eq!(back.direction, Some("right"));
    }
}
