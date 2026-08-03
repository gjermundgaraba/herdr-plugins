use crate::{
    actions::{Agent, CHATGPT_BUNDLE_IDS},
    config::{AgentStatus, Light, LightingConfig},
};
use std::collections::{HashMap, HashSet};

pub const SLOT_COUNT: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub struct JoystickEvent {
    pub sector: Option<u8>,
    pub direction: Option<&'static str>,
}

pub fn device_owner(processes: &[String], frontmost_bundle: Option<&str>) -> Option<&'static str> {
    let native_frontmost = CHATGPT_BUNDLE_IDS
        .into_iter()
        .find(|bundle| crate::macos::frontmost_bundle_is(bundle));
    device_owner_with_bundles(
        &crate::macos::running_bundle_ids(),
        processes,
        native_frontmost.or(frontmost_bundle),
    )
}

pub fn device_owner_with_bundles(
    running_bundles: &[String],
    processes: &[String],
    frontmost_bundle: Option<&str>,
) -> Option<&'static str> {
    let input_running = bundle_is_running(running_bundles, "it.focusense.input-app")
        || has_process(processes, "input", "input");
    if input_running {
        return Some("Input");
    }

    let frontmost_openai =
        frontmost_bundle.is_some_and(|bundle| CHATGPT_BUNDLE_IDS.contains(&bundle));
    let chatgpt_running = CHATGPT_BUNDLE_IDS
        .iter()
        .any(|bundle| bundle_is_running(running_bundles, bundle))
        || has_process(processes, "ChatGPT", "ChatGPT")
        || has_process(processes, "Codex", "ChatGPT");
    (frontmost_openai && chatgpt_running).then_some("ChatGPT")
}

fn bundle_is_running(bundles: &[String], expected: &str) -> bool {
    bundles.iter().any(|bundle| bundle == expected)
}

fn has_process(processes: &[String], app: &str, executable: &str) -> bool {
    let suffix = format!("/{app}.app/Contents/MacOS/{executable}");
    processes.iter().any(|command| {
        command
            .split_ascii_whitespace()
            .next()
            .is_some_and(|path| path.ends_with(&suffix))
    })
}
fn priority(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Unknown => 0,
        AgentStatus::Idle => 1,
        AgentStatus::Working => 2,
        AgentStatus::Done => 3,
        AgentStatus::Blocked => 4,
    }
}
fn compare(a: &Agent, b: &Agent) -> std::cmp::Ordering {
    priority(b.agent_status)
        .cmp(&priority(a.agent_status))
        .then_with(|| b.state_change_seq.cmp(&a.state_change_seq))
}
pub fn assign_slots(previous: &[Option<String>], agents: &[Agent]) -> Vec<Option<String>> {
    let mut sorted = agents.to_vec();
    sorted.sort_by(compare);
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
            slotted.insert(candidate.terminal_id);
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
        if priority(candidate.agent_status) <= priority(displaced.agent_status) {
            break;
        }
        slotted.remove(&displaced.terminal_id);
        slots[victim] = Some(candidate.terminal_id.clone());
        slotted.insert(candidate.terminal_id);
    }
    slots
}
pub fn slot_lighting(
    slots: &[Option<String>],
    agents: &[Agent],
    config: &LightingConfig,
) -> Vec<Light> {
    let by_id: HashMap<_, _> = agents.iter().map(|a| (a.terminal_id.as_str(), a)).collect();
    slots
        .iter()
        .map(|slot| {
            slot.as_ref()
                .and_then(|id| by_id.get(id.as_str()))
                .map(|a| {
                    let mut l = config.light(a.agent_status);
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
    agents: &[Agent],
    config: &LightingConfig,
) -> HashMap<String, Light> {
    let by_id: HashMap<_, _> = agents.iter().map(|a| (a.terminal_id.as_str(), a)).collect();
    let best = slots
        .iter()
        .flatten()
        .filter_map(|id| by_id.get(id.as_str()))
        .min_by(|a, b| compare(a, b));
    let light = best.map(|a| config.light(a.agent_status)).unwrap_or(Light {
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
    use crate::config::default_lighting;
    fn agent(id: &str, status: AgentStatus) -> Agent {
        Agent {
            terminal_id: id.into(),
            pane_id: String::new(),
            agent: String::new(),
            agent_status: status,
            state_change_seq: 0,
            focused: false,
            cwd: String::new(),
        }
    }
    #[test]
    fn sticky_slots_lighting_and_joystick() {
        let slots = assign_slots(
            &[],
            &[
                agent("idle", AgentStatus::Idle),
                agent("working", AgentStatus::Working),
            ],
        );
        assert_eq!(slots[..2], [Some("working".into()), Some("idle".into())]);
        assert_eq!(
            joystick_event(0.25, 0.9, Some(0), 0.75, 0.3).direction,
            Some("down")
        );
        assert_eq!(joystick_event(0.0, 0.1, Some(0), 0.75, 0.3).sector, None);
        assert_eq!(
            slot_lighting(
                &slots,
                &[
                    agent("idle", AgentStatus::Idle),
                    agent("working", AgentStatus::Working)
                ],
                &default_lighting()
            )
            .len(),
            SLOT_COUNT
        );
    }
    #[test]
    fn device_owners_use_bundle_identity_with_relocated_process_fallback() {
        let input = "/Users/me/Applications/input.app/Contents/MacOS/input --background".to_owned();
        assert_eq!(
            device_owner_with_bundles(&[], &[input], None),
            Some("Input")
        );

        let chatgpt = "/Volumes/Tools/ChatGPT.app/Contents/MacOS/ChatGPT --launch".to_owned();
        assert_eq!(
            device_owner_with_bundles(&["com.openai.codex".into()], &[], Some("com.openai.codex")),
            Some("ChatGPT")
        );
        assert_eq!(
            device_owner_with_bundles(&["com.openai.chat".into()], &[], Some("com.openai.chat")),
            Some("ChatGPT")
        );
        assert_eq!(
            device_owner_with_bundles(&[], &[chatgpt], Some("com.openai.codex")),
            Some("ChatGPT")
        );
        assert_eq!(
            device_owner_with_bundles(&["com.openai.codex".into()], &[], Some("com.example.other")),
            None
        );
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
