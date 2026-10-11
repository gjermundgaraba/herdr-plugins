//! Lighting derived from the followed client's report and the sticky slots,
//! and the joystick's direction decoding.

use crate::config::{AgentStatus, Direction, Light, LightingConfig};
use codex_micro::service::Lighting;
use herdr_client::{
    ClientReportAgent,
    slots::{SlotKey, first_in_priority},
};

pub const SLOT_COUNT: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub struct JoystickEvent {
    pub sector: Option<u8>,
    pub direction: Option<Direction>,
}

fn status(row: &ClientReportAgent) -> AgentStatus {
    match row.agent.agent_status.as_str() {
        "idle" => AgentStatus::Idle,
        "working" => AgentStatus::Working,
        "blocked" => AgentStatus::Blocked,
        "done" => AgentStatus::Done,
        _ => AgentStatus::Unknown,
    }
}

/// The row behind one of the six sticky Agent keys.
pub fn slot_row<'a>(
    slot: Option<&SlotKey>,
    rows: &'a [ClientReportAgent],
) -> Option<&'a ClientReportAgent> {
    let key = slot?;
    rows.iter().find(|row| key.is(row))
}

/// The complete lighting for the slotted agents. The aggregate zones show the
/// status of the slotted agent that is first in the daemon's priority order.
pub fn lighting(
    slots: &[Option<SlotKey>],
    rows: &[ClientReportAgent],
    config: &LightingConfig,
) -> Lighting {
    let slotted = |index: usize| slot_row(slots.get(index).and_then(Option::as_ref), rows);
    // Report order, so equal priority keys keep the report's tie-break.
    let light = first_in_priority(
        rows.iter()
            .filter(|row| slots.iter().flatten().any(|key| key.is(row))),
    )
    .map(|row| config.light(status(row)))
    .unwrap_or_default();
    Lighting {
        ambient: if config.ambient {
            light
        } else {
            Light::default()
        },
        keys: if config.keys { light } else { Light::default() },
        slots: std::array::from_fn(|index| {
            slotted(index)
                .map(|row| {
                    let mut light = config.light(status(row));
                    // The row's own focus is the client's: one agent at most.
                    if row.focused {
                        light.b = light.b.max(config.focused_brightness);
                    }
                    light
                })
                .unwrap_or_default()
        }),
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
pub(crate) mod tests {
    use super::*;
    use crate::config::Config;
    use herdr_client::{ClientList, ClientReport, slots::assign_slots};

    /// The report of the fork's canonical `client.list` response.
    pub(crate) fn fixture_report() -> ClientReport {
        let response: serde_json::Value = serde_json::from_str(include_str!(
            "../../sdk/rust/tests/fixtures/api-client-list-v1.json"
        ))
        .unwrap();
        let list: ClientList = serde_json::from_value(response["result"].clone()).unwrap();
        list.clients[0].report.clone().unwrap()
    }

    /// A row of the fixture with another pane, status, and priority key.
    pub(crate) fn row(pane: &str, status: &str, priority_key: &str) -> ClientReportAgent {
        let mut row = fixture_report().agents[0].clone();
        row.focused = false;
        row.agent.pane_id = pane.into();
        row.agent.agent_status = status.into();
        row.agent.priority_key = priority_key.into();
        row.agent.focused = false;
        row
    }

    fn key(endpoint: &str, pane: &str) -> Option<SlotKey> {
        Some(SlotKey {
            endpoint_id: endpoint.into(),
            pane_id: pane.into(),
        })
    }

    #[test]
    fn slots_fill_in_the_daemons_priority_order_and_light_each_status() {
        let report = fixture_report();
        let slots = assign_slots(&[], &report.agents, SLOT_COUNT, |_| true);
        // Report order is done, blocked, working (remote), idle; the keys say
        // the same, and the remote agent keeps its endpoint in its identity.
        assert_eq!(
            slots,
            [
                key("local", "w1:p1"),
                key("local", "w2:p1"),
                key("ssh:0123456789abcdef0123456789abcdef", "w1:p4"),
                key("local", "w2:p2"),
                None,
                None
            ]
        );

        let config = Config::default().lighting;
        let lights = lighting(&slots, &report.agents, &config);
        let mut focused_blocked = config.light(AgentStatus::Blocked);
        focused_blocked.b = focused_blocked.b.max(config.focused_brightness);
        assert_eq!(
            lights.slots,
            [
                config.light(AgentStatus::Done),
                // Only the row the client has focused is highlighted; the
                // remote agent is focused inside its endpoint, not here.
                focused_blocked,
                config.light(AgentStatus::Working),
                config.light(AgentStatus::Idle),
                Light::default(),
                Light::default(),
            ]
        );
        // The aggregate follows the first agent in priority order: the done
        // agent of a priority space, ahead of the blocked one.
        assert_eq!(lights.ambient, config.light(AgentStatus::Done));
        assert_eq!(lights.keys, Light::default());

        let mut idle_focused = report.agents.clone();
        idle_focused[1].focused = false;
        idle_focused[3].focused = true;
        let mut bright = config.light(AgentStatus::Idle);
        bright.b = config.focused_brightness;
        assert_ne!(bright, config.light(AgentStatus::Idle));
        assert_eq!(lighting(&slots, &idle_focused, &config).slots[3], bright);
    }

    #[test]
    fn disabling_aggregates_produces_explicit_off_in_complete_snapshot() {
        let rows = [row("working", "working", "1.2.0000")];
        let slots = assign_slots(&[], &rows, SLOT_COUNT, |_| true);
        let mut config = Config::default().lighting;
        config.ambient = true;
        config.keys = true;
        let lit = lighting(&slots, &rows, &config);
        assert_ne!(lit.ambient, Light::default());
        assert_ne!(lit.keys, Light::default());
        config.ambient = false;
        config.keys = false;
        let disabled = lighting(&slots, &rows, &config);
        assert_eq!(disabled.ambient, Light::default());
        assert_eq!(disabled.keys, Light::default());
        assert_eq!(disabled.slots, lit.slots);
        assert_eq!(lighting(&[], &rows, &config), Lighting::default());
    }

    #[test]
    fn joystick_engages_and_releases_by_distance() {
        assert_eq!(
            joystick_event(0.25, 0.9, Some(0), 0.75, 0.3).direction,
            Some(Direction::Down)
        );
        assert_eq!(joystick_event(0.0, 0.1, Some(0), 0.75, 0.3).sector, None);
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
