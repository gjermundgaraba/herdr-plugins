use crate::{
    actions::Agent,
    config::{AgentStatus, Light, LightingConfig},
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub const SLOT_COUNT: usize = 6;
pub const REPORT_ID: u8 = 6;
pub const CHANNEL_RPC: u8 = 2;
pub const REPORT_SIZE: usize = 64;
pub const MAX_PAYLOAD: usize = 61;
pub const MAX_REASSEMBLED: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct JoystickEvent {
    pub sector: Option<u8>,
    pub direction: Option<&'static str>,
}

pub fn device_owner(processes: &[String], frontmost_process: Option<&str>) -> Option<&'static str> {
    const OWNERS: [(&str, Option<&str>); 3] = [
        ("/Applications/input.app/Contents/MacOS/input", None),
        (
            "/Applications/Codex.app/Contents/MacOS/ChatGPT",
            Some("com.openai.codex"),
        ),
        (
            "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT",
            Some("com.openai.codex"),
        ),
    ];
    OWNERS
        .into_iter()
        .find(|(path, process)| {
            processes.iter().any(|p| p == path)
                && (process.is_none() || *process == frontmost_process)
        })
        .map(|(path, _)| path)
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
pub fn encode_message(
    method: &str,
    params: Option<&Value>,
    id: Option<u64>,
) -> Result<Vec<[u8; REPORT_SIZE]>, serde_json::Error> {
    let mut envelope = serde_json::Map::new();
    envelope.insert("m".into(), Value::String(method.into()));
    if let Some(params) = params {
        envelope.insert("p".into(), params.clone());
    }
    if let Some(id) = id {
        envelope.insert("id".into(), Value::from(id));
    }
    let mut bytes = serde_json::to_vec(&Value::Object(envelope))?;
    bytes.extend_from_slice(b"\r\n");
    Ok(bytes
        .chunks(MAX_PAYLOAD)
        .map(|chunk| {
            let mut report = [0; REPORT_SIZE];
            report[0] = REPORT_ID;
            report[1] = CHANNEL_RPC;
            report[2] = chunk.len() as u8;
            report[3..3 + chunk.len()].copy_from_slice(chunk);
            report
        })
        .collect())
}
#[derive(Default, Debug)]
pub struct Reassembler {
    buffer: Vec<u8>,
}
impl Reassembler {
    pub fn push(&mut self, report: &[u8]) -> Vec<Result<Value, serde_json::Error>> {
        let offset = if report.get(0..2) == Some(&[REPORT_ID, CHANNEL_RPC]) {
            1
        } else if report.first() == Some(&CHANNEL_RPC) {
            0
        } else {
            return vec![];
        };
        if report.len() < offset + 2 {
            return vec![];
        }
        let length = report[offset + 1] as usize;
        if length > MAX_PAYLOAD || length + offset + 2 > report.len() {
            return vec![];
        }
        self.buffer
            .extend_from_slice(&report[offset + 2..offset + 2 + length]);
        let mut messages = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.buffer.drain(..=newline).collect();
            messages.push(serde_json::from_slice(&line[..line.len() - 1]));
        }
        if self.buffer.len() > MAX_REASSEMBLED {
            self.buffer.clear();
        }
        messages
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
    fn chatgpt_owns_the_device_only_while_frontmost() {
        let process = "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT".to_owned();
        assert_eq!(
            device_owner(std::slice::from_ref(&process), Some("com.openai.codex")),
            Some(process.as_str())
        );
        assert_eq!(
            device_owner(std::slice::from_ref(&process), Some("com.openai.chat")),
            None
        );
    }
    #[test]
    fn accepts_report_id_variants_and_rejects_bad_reports() {
        let params = serde_json::json!({"ok":true});
        let reports = encode_message("event", Some(&params), Some(1)).unwrap();
        let mut reassembler = Reassembler::default();
        assert_eq!(
            reassembler.push(&reports[0])[0].as_ref().unwrap()["m"],
            "event"
        );
        assert_eq!(Reassembler::default().push(&reports[0][1..]).len(), 1);
        assert!(Reassembler::default()
            .push(&[REPORT_ID, CHANNEL_RPC, 62])
            .is_empty());
        let mut over = Reassembler::default();
        let mut fragment = vec![CHANNEL_RPC, 61];
        fragment.extend(std::iter::repeat(b'x').take(61));
        for _ in 0..1100 {
            over.push(&fragment);
        }
        assert!(over.buffer.len() < MAX_REASSEMBLED);
    }
}
