//! Bridge state and its reconciliation with the followed Herdr client and
//! the HID device.

use codex_micro::{ExternalOwner, external_owner, service::ServiceStatus};
use herdr_client::{
    ClientReportAgent,
    follow::{ClientKey, DaemonState, Directory, JumpTarget},
    slots::{SlotKey, sticky_slots},
};
use herdr_device::{Focus, selection_change};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use crate::{
    actions::HERDR_LAYER,
    config::{Config, Controls, enabled_buttons},
    protocol::{SLOT_COUNT, lighting, slot_row},
};

use super::{DAEMON_PROTOCOL_VERSION, device, log, log_changed};

pub(super) struct State {
    pub(super) config: Config,
    /// Every followed Herdr daemon and its attached clients.
    pub(super) directory: Directory,
    pub(super) routing_ready: bool,
    /// The followed client's agents a jump can reach, in the report's order.
    pub(super) rows: Vec<ClientReportAgent>,
    /// The agents behind the six keys. One that is in the report and cannot
    /// be reached now keeps its key and is not in `rows`: the key is dark
    /// and jumps nowhere until its endpoint is back.
    pub(super) slots: Vec<Option<SlotKey>>,
    /// The client `slots` belong to. They survive a time when no client is
    /// followed, so its keys mean the same when it is followed again.
    slots_client: Option<ClientKey>,
    pub(super) device_state: String,
    pub(super) owner: Option<ExternalOwner>,
    pub(super) active_layer: Option<usize>,
    pub(super) last_device_error: String,
    pub(super) last_controls_error: String,
}

impl State {
    pub(super) fn new(config: Config) -> Self {
        Self {
            config,
            directory: Directory::default(),
            routing_ready: false,
            rows: Vec::new(),
            slots: vec![None; SLOT_COUNT],
            slots_client: None,
            device_state: "starting".into(),
            owner: None,
            active_layer: None,
            last_device_error: String::new(),
            last_controls_error: String::new(),
        }
    }

    pub(super) fn status(&self) -> Value {
        let focused = self.directory.followed_client();
        json!({
            "device": if self.owner.is_some() { "yielded" } else { &self.device_state },
            "deviceError": (!self.last_device_error.is_empty()).then_some(&self.last_device_error),
            "owner": self.owner.map(|owner| owner.to_string()),
            "session": focused.map(|client| client.session),
            "client": focused.map(|client| client.client_id),
            "clientFocused": focused.map(|client| client.has_focus()),
            "routing": if self.routing_ready { "ready" } else if focused.is_some() { "unavailable" } else { "none" },
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": DAEMON_PROTOCOL_VERSION,
            "daemons": self.directory.daemons.iter().map(|daemon| json!({
                "session": daemon.session,
                "state": match daemon.state {
                    DaemonState::Following { .. } => "following",
                    DaemonState::Offline => "offline",
                },
                "clients": daemon.clients.len(),
            })).collect::<Vec<_>>(),
            "endpoints": focused.map(|client| client.report.endpoints.iter().map(|endpoint| json!({
                "id": endpoint.endpoint_id, "label": endpoint.label, "status": endpoint.status,
            })).collect::<Vec<_>>()),
            "layer": self.active_layer,
            "agents": self.rows.len(),
            "focusedAgent": focused.and_then(|client| client.focused_agent()).map(|row| json!({
                "endpoint": row.endpoint_id,
                "pane": row.agent.pane_id,
                "agent": row.agent.agent,
            })),
            "slots": self.slots.iter().map(|slot| {
                slot_row(slot.as_ref(), &self.rows).map(|row| json!({
                    "endpoint": row.endpoint_id,
                    "pane": row.agent.pane_id,
                    "agent": row.agent.agent,
                    "status": row.agent.agent_status,
                }))
            }).collect::<Vec<_>>(),
        })
    }

    pub(super) fn disable_routing(&mut self) {
        self.routing_ready = false;
    }
}

/// What the input thread acts on: the latest controls, and the destinations
/// that exist while routing is ready.
#[derive(Clone)]
pub(super) struct InputContext {
    pub(super) controls: Controls,
    /// The followed client, captured at a press. Prompts and submits are
    /// keyboard events for the frontmost app, sent while [`Focus`] still
    /// names this client and says its window has focus.
    pub(super) client: Option<ClientKey>,
    /// The kind of agent that client has focused, for `byAgent` bindings.
    pub(super) agent: Option<String>,
    /// The jump behind each Agent key.
    pub(super) slots: Vec<Option<JumpTarget>>,
}

impl InputContext {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            controls: config.controls.clone(),
            client: None,
            agent: None,
            slots: vec![None; SLOT_COUNT],
        }
    }
}

pub(super) fn apply_service_status(state: &mut State, status: &ServiceStatus) -> bool {
    let next_state = if status.device.is_some() {
        "connected"
    } else {
        "unavailable"
    };
    let next_error = status.last_error.as_deref().unwrap_or_default();
    let changed = state.device_state != next_state || state.last_device_error != next_error;
    state.device_state = next_state.into();
    state.last_device_error = next_error.into();
    changed
}

/// Compute desired output from light-affecting state only. The device worker
/// compares this value, so title, token, and order changes never resend LEDs.
pub(super) fn device_output(state: &State) -> device::Output {
    device::Output {
        layer: state.active_layer,
        lighting: if state.directory.followed_client().is_none() {
            Default::default()
        } else {
            lighting(&state.slots, &state.rows, &state.config.lighting)
        },
    }
}

pub(super) fn reconcile_active(state: &mut State, stopping: &AtomicBool) {
    let focused = state.directory.followed_client();
    state.rows = focused
        .map(|client| client.reachable_agents().cloned().collect())
        .unwrap_or_default();
    if let Some(client) = focused {
        // Every reported agent keeps its slot; only one a jump can reach
        // takes a free one.
        state.slots = sticky_slots(
            &state.slots,
            &mut state.slots_client,
            client.key(),
            &client.report.agents,
            SLOT_COUNT,
            |row| client.can_reach(row),
        );
    }
    if focused.is_none() || state.owner.is_some() || stopping.load(Ordering::Acquire) {
        state.disable_routing();
    } else {
        state.active_layer = Some(HERDR_LAYER);
        state.routing_ready = true;
    }
}

pub(super) fn apply_clients_update(state: &mut State, directory: Directory, stopping: &AtomicBool) {
    if let Some(line) = selection_change(&state.directory, &directory) {
        log(line);
    }
    state.directory = directory;
    reconcile_active(state, stopping);
}

pub(super) fn refresh_owner(state: &mut State) -> bool {
    let owner = external_owner();
    if owner != state.owner {
        state.owner = owner;
        if let Some(owner) = state.owner {
            log(format!("device owned by {owner}"));
            state.disable_routing();
        } else {
            log("device owner cleared");
        }
        true
    } else {
        false
    }
}

pub(super) fn publish(status: &Arc<Mutex<Value>>, state: &State) {
    *status.lock().unwrap_or_else(|error| error.into_inner()) = state.status();
}

/// Publishes routing to the input threads: the context presses capture, and
/// the followed client and its window focus that dispatch checks.
pub(super) fn update_input_context(
    context: &Mutex<Arc<InputContext>>,
    focus: &Focus,
    state: &State,
) {
    let focused = state
        .routing_ready
        .then(|| state.directory.followed_client())
        .flatten();
    focus.follow(focused);
    let next = InputContext {
        controls: state.config.controls.clone(),
        client: focused.map(|client| client.key()),
        agent: focused
            .and_then(|client| client.focused_agent())
            .and_then(|row| row.agent.agent.clone()),
        slots: state
            .slots
            .iter()
            .map(|slot| focused?.jump_to(slot_row(slot.as_ref(), &state.rows)?))
            .collect(),
    };
    *context.lock().unwrap_or_else(|error| error.into_inner()) = Arc::new(next);
}

pub(super) fn handle_input_disconnect(state: &mut State, error: String) {
    state.device_state = "unavailable".into();
    log_changed(&mut state.last_device_error, error, "device disconnected: ");
    state.disable_routing();
}

pub(super) fn apply_config_load(
    state: &mut State,
    loaded: std::result::Result<Config, String>,
    startup_enabled_buttons: [bool; 7],
) -> bool {
    match loaded {
        Ok(next) => {
            let pending = (enabled_buttons(&next.controls) != startup_enabled_buttons)
                .then_some("button enable changes require micro-setup and a bridge restart");
            let error_changed = state.last_controls_error != pending.unwrap_or_default();
            if error_changed {
                if let Some(pending) = pending {
                    log(format!("control configuration pending: {pending}"));
                }
                state.last_controls_error = pending.unwrap_or_default().into();
            }
            let config_changed = state.config != next;
            if config_changed {
                state.config = next;
            }
            config_changed
        }
        Err(error) => {
            if state.last_controls_error == error {
                return false;
            }
            state.last_controls_error = error.clone();
            log(format!("control configuration failed: {error}"));
            false
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::config::Light;
    use crate::protocol::tests::{fixture_report, row};
    use herdr_client::{
        ClientEndpointStatus, ClientInfo, ClientJumpParams, ClientReport, follow::DaemonClients,
    };

    pub(in crate::daemon) fn daemon(session: &str, clients: Vec<ClientInfo>) -> DaemonClients {
        DaemonClients {
            session: session.into(),
            socket_path: format!("/run/herdr/{session}/herdr.sock").into(),
            state: DaemonState::Following {
                boot_id: format!("daemon-boot-{session}"),
            },
            clients,
        }
    }

    pub(in crate::daemon) fn client(client_id: u64, focused: Option<bool>) -> ClientInfo {
        let mut report = fixture_report();
        report.focused = focused;
        ClientInfo {
            client_id,
            report: Some(report),
        }
    }

    /// The follower's next directory: `daemons` after what `state` holds.
    fn apply(state: &mut State, daemons: Vec<DaemonClients>) {
        let directory = state.directory.updated(daemons);
        apply_clients_update(state, directory, &AtomicBool::new(false));
    }

    fn input_context(state: &State) -> Arc<InputContext> {
        let shared = Mutex::new(Arc::new(InputContext::new(&state.config)));
        let focus = Focus::new(Arc::default());
        update_input_context(&shared, &focus, state);
        let context = shared.into_inner().unwrap();
        // Dispatch checks the same client that presses capture, and types
        // only while that client's window has focus.
        match &context.client {
            Some(client) => {
                assert_eq!(focus.check_followed(client), Ok(()));
                let focused = state.directory.followed_client().unwrap().has_focus();
                assert_eq!(
                    focus.check_focused(client),
                    if focused {
                        Ok(())
                    } else {
                        Err(herdr_device::Refusal::NotFocused)
                    }
                );
            }
            None => assert_eq!(
                focus.check_followed(&ClientKey {
                    socket_path: "/run/herdr/default/herdr.sock".into(),
                    boot_id: "daemon-boot-default".into(),
                    client_id: 7,
                }),
                Err(herdr_device::Refusal::NotFollowed)
            ),
        }
        context
    }

    fn report_of(client: &mut ClientInfo) -> &mut ClientReport {
        client.report.as_mut().unwrap()
    }

    #[test]
    fn no_followed_client_blanks_lights_and_routes_nothing() {
        let mut state = State::new(Config::default());
        let silent = ClientInfo {
            client_id: 9,
            report: None,
        };
        // No client has sent a report.
        apply(&mut state, vec![daemon("default", vec![silent.clone()])]);
        assert!(!state.routing_ready);
        assert_eq!(device_output(&state).lighting, Default::default());

        // The followed client detaches and no other reports, or its daemon
        // stops answering: then it is kept, but there is nothing to show of
        // it, and no other client is shown in its place.
        let mut offline = daemon("default", vec![]);
        offline.state = DaemonState::Offline;
        for daemons in [
            vec![],
            vec![daemon("default", vec![])],
            vec![daemon("default", vec![silent.clone()])],
            vec![offline.clone()],
            vec![offline, daemon("work", vec![client(3, Some(false))])],
        ] {
            // Start from a lit, routed state each time.
            apply(
                &mut state,
                vec![daemon("default", vec![client(7, Some(true))])],
            );
            assert!(state.routing_ready);
            assert_ne!(device_output(&state).lighting, Default::default());

            apply(&mut state, daemons);
            assert!(!state.routing_ready);
            assert!(state.rows.is_empty());
            assert_eq!(device_output(&state).lighting, Default::default());
            assert!(
                state.status()["slots"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(serde_json::Value::is_null)
            );
            let context = input_context(&state);
            assert!(context.client.is_none() && context.agent.is_none());
            assert!(context.slots.iter().all(Option::is_none));
            assert_eq!(state.status()["routing"], "none");
            assert_eq!(state.status()["clientFocused"], Value::Null);
        }
    }

    #[test]
    fn a_client_that_never_reported_focus_is_followed_when_no_other_took_it() {
        // A TUI reports no focus until its terminal sends a focus change.
        // Its agents light up and its Agent keys jump; `input_context`
        // asserts that typing is refused.
        for focused in [None, Some(false)] {
            let mut state = State::new(Config::default());
            apply(
                &mut state,
                vec![daemon("default", vec![client(7, focused)])],
            );
            assert!(state.routing_ready);
            assert_ne!(device_output(&state).lighting, Default::default());
            let status = state.status();
            assert_eq!(status["client"], 7);
            assert_eq!(status["clientFocused"], false);
            let context = input_context(&state);
            assert!(context.client.is_some());
            assert!(context.slots.iter().any(Option::is_some));
        }

        // The followed client detaches: another reporting client is
        // followed in its place.
        let mut state = State::new(Config::default());
        apply(
            &mut state,
            vec![daemon(
                "default",
                vec![client(7, Some(true)), client(8, Some(false))],
            )],
        );
        assert_eq!(state.status()["client"], 7);
        apply(
            &mut state,
            vec![daemon("default", vec![client(8, Some(false))])],
        );
        assert!(state.routing_ready);
        assert_eq!(state.status()["client"], 8);
    }

    #[test]
    fn another_app_in_front_keeps_lights_slots_and_jumps() {
        let mut state = State::new(Config::default());
        apply(
            &mut state,
            vec![daemon("default", vec![client(7, Some(true))])],
        );
        let lights = device_output(&state);
        let slots = state.slots.clone();
        let jumps = input_context(&state).slots.clone();
        assert_ne!(lights.lighting, Default::default());

        // The window loses focus, or stops knowing whether it has it.
        for focused in [Some(false), None] {
            apply(
                &mut state,
                vec![daemon("default", vec![client(7, focused)])],
            );
            assert!(state.routing_ready);
            assert_eq!(device_output(&state), lights);
            assert_eq!(state.slots, slots);
            let status = state.status();
            assert_eq!(status["routing"], "ready");
            assert_eq!(status["client"], 7);
            assert_eq!(status["clientFocused"], false);
            // Presses still capture the client and its jumps; typing is
            // refused at dispatch, which `input_context` asserts.
            let context = input_context(&state);
            assert_eq!(context.slots, jumps);
            assert!(context.client.is_some());
        }
    }

    #[test]
    fn the_latest_client_to_take_focus_wins() {
        let mut state = State::new(Config::default());
        let mut work = client(2, Some(true));
        report_of(&mut work).agents.truncate(1);
        apply(
            &mut state,
            vec![daemon("default", vec![client(7, Some(true))])],
        );
        // Two in one daemon, and one in each of two daemons.
        apply(
            &mut state,
            vec![daemon(
                "default",
                vec![client(7, Some(true)), client(8, Some(true))],
            )],
        );
        assert_eq!(state.status()["client"], 8);
        apply(
            &mut state,
            vec![
                daemon(
                    "default",
                    vec![client(7, Some(true)), client(8, Some(true))],
                ),
                daemon("work", vec![work]),
            ],
        );
        assert!(state.routing_ready);
        assert_eq!(state.status()["session"], "work");
        assert_eq!(state.rows.len(), 1);
    }

    #[test]
    fn the_followed_client_supplies_slots_lights_and_jump_targets() {
        let mut state = State::new(Config::default());
        // The `work` client took focus; other daemons' clients are ignored.
        apply(
            &mut state,
            vec![
                daemon("default", vec![client(3, Some(false)), client(4, None)]),
                daemon("work", vec![client(7, Some(true))]),
            ],
        );
        assert!(state.routing_ready);
        assert_eq!(state.active_layer, Some(HERDR_LAYER));
        assert_eq!(state.rows, fixture_report().agents);
        let output = device_output(&state);
        assert_eq!(
            output.lighting,
            lighting(
                &state.slots,
                &fixture_report().agents,
                &state.config.lighting
            )
        );
        assert_eq!(output.layer, Some(HERDR_LAYER));

        let context = input_context(&state);
        let key = ClientKey {
            socket_path: "/run/herdr/work/herdr.sock".into(),
            boot_id: "daemon-boot-work".into(),
            client_id: 7,
        };
        assert_eq!(context.client.as_ref(), Some(&key));
        // The focused row of the fixture is a Claude agent.
        assert_eq!(context.agent.as_deref(), Some("claude"));
        // Each slot jumps with its row's endpoint and that endpoint's boot.
        assert_eq!(
            context
                .slots
                .iter()
                .map(|slot| slot.as_ref().map(JumpTarget::params))
                .collect::<Vec<_>>(),
            [
                Some(("local", "boot-a", "w1:p1")),
                Some(("local", "boot-a", "w2:p1")),
                Some(("ssh:0123456789abcdef0123456789abcdef", "boot-b", "w1:p4")),
                Some(("local", "boot-a", "w2:p2")),
                None,
                None,
            ]
            .map(|slot| slot.map(|(endpoint, boot, pane)| ClientJumpParams {
                client_id: 7,
                endpoint_id: endpoint.into(),
                boot_id: boot.into(),
                pane_id: pane.into(),
            }))
        );
        assert!(
            context
                .slots
                .iter()
                .flatten()
                .all(|slot| slot.client == key)
        );

        let status = state.status();
        assert_eq!(status["session"], "work");
        assert_eq!(status["client"], 7);
        assert_eq!(status["clientFocused"], true);
        assert_eq!(status["routing"], "ready");
        assert_eq!(status["agents"], 4);
        assert_eq!(
            status["focusedAgent"],
            json!({"endpoint": "local", "pane": "w2:p1", "agent": "claude"})
        );
        assert_eq!(status["slots"][2]["status"], "working");
        assert_eq!(status["daemons"][1]["state"], "following");
    }

    #[test]
    fn focus_moving_to_another_daemon_moves_routing_with_it() {
        let mut state = State::new(Config::default());
        let mut work = client(2, Some(false));
        report_of(&mut work).agents.truncate(1);
        apply(
            &mut state,
            vec![
                daemon("default", vec![client(7, Some(true))]),
                daemon("work", vec![work.clone()]),
            ],
        );
        assert_eq!(state.rows.len(), 4);
        assert_eq!(state.status()["session"], "default");

        report_of(&mut work).focused = Some(true);
        apply(
            &mut state,
            vec![
                daemon("default", vec![client(7, Some(false))]),
                daemon("work", vec![work]),
            ],
        );
        assert!(state.routing_ready);
        assert_eq!(state.rows.len(), 1);
        let context = input_context(&state);
        assert_eq!(
            context.client.as_ref().map(|client| client.client_id),
            Some(2)
        );
        assert_eq!(
            context.slots.iter().flatten().count(),
            1,
            "only the new client's agent keeps a slot"
        );
        assert_eq!(state.status()["session"], "work");
    }

    #[test]
    fn an_agent_a_jump_cannot_reach_takes_no_slot() {
        let mut state = State::new(Config::default());
        let mut focused = client(7, Some(true));
        let report = report_of(&mut focused);
        // The remote endpoint drops out; a third one never reported a boot.
        report.endpoints[1].status = ClientEndpointStatus::RECONNECTING.into();
        let mut pending = row("w9:p9", "blocked", "1.0.0000");
        pending.endpoint_id = report.endpoints[2].endpoint_id.clone();
        report.endpoints[2].status = ClientEndpointStatus::ONLINE.into();
        report.agents.push(pending);
        apply(&mut state, vec![daemon("default", vec![focused])]);
        assert_eq!(
            state
                .rows
                .iter()
                .map(|row| row.endpoint_id.as_str())
                .collect::<Vec<_>>(),
            ["local", "local", "local"]
        );
        assert_eq!(input_context(&state).slots.iter().flatten().count(), 3);
    }

    #[test]
    fn a_reconnecting_endpoint_keeps_its_agents_keys_dark_until_it_is_back() {
        let mut state = State::new(Config::default());
        let mut focused = client(7, Some(true));
        apply(&mut state, vec![daemon("default", vec![focused.clone()])]);
        let slots = state.slots.clone();
        let lights = device_output(&state).lighting;
        let jumps = input_context(&state).slots.clone();
        // The third key is the agent of the remote endpoint.
        let remote = slots[2].clone().expect("the remote agent has a key");
        assert_eq!(
            remote.endpoint_id,
            report_of(&mut focused).endpoints[1].endpoint_id
        );
        assert_ne!(lights.slots[2], Light::default());
        assert!(jumps[2].is_some());

        // The endpoint reconnects. The client still reports its agent, and
        // a new local agent besides.
        let report = report_of(&mut focused);
        report.endpoints[1].status = ClientEndpointStatus::RECONNECTING.into();
        report.agents.push(row("w9:p9", "blocked", "1.0.0000"));
        apply(&mut state, vec![daemon("default", vec![focused.clone()])]);
        // The key is still the remote agent's: the newcomer got a free one.
        assert_eq!(state.slots[..4], slots[..4]);
        assert_eq!(
            state.slots[4].as_ref().map(|key| key.pane_id.as_str()),
            Some("w9:p9")
        );
        // It is dark, it jumps nowhere, and the other keys are as before.
        let dark = device_output(&state).lighting;
        let context = input_context(&state);
        assert_eq!(dark.slots[2], Light::default());
        assert_eq!(context.slots[2], None);
        assert_eq!(state.status()["slots"][2], Value::Null);
        for key in [0, 1, 3] {
            assert_eq!(dark.slots[key], lights.slots[key]);
            assert_eq!(context.slots[key], jumps[key]);
        }

        // The endpoint is back: the key is lit and jumps to the same agent.
        report_of(&mut focused).endpoints[1].status = ClientEndpointStatus::ONLINE.into();
        apply(&mut state, vec![daemon("default", vec![focused.clone()])]);
        assert_eq!(state.slots[..4], slots[..4]);
        assert_eq!(device_output(&state).lighting.slots[..4], lights.slots[..4]);
        assert_eq!(input_context(&state).slots[..4], jumps[..4]);

        // An agent that leaves the report while its endpoint reconnects
        // gives its key up.
        let report = report_of(&mut focused);
        report.endpoints[1].status = ClientEndpointStatus::RECONNECTING.into();
        report.agents.retain(|row| !remote.is(row));
        apply(&mut state, vec![daemon("default", vec![focused])]);
        assert_eq!(state.slots[2], None);
    }

    #[test]
    fn sticky_slots_survive_title_churn_and_only_status_changes_relight() {
        let mut state = State::new(Config::default());
        let mut focused = client(7, Some(true));
        let template = row("p", "idle", "1.3.0000");
        report_of(&mut focused).agents = (0..8)
            .map(|n| {
                let mut row = template.clone();
                row.agent.pane_id = format!("p{n}");
                row.agent.priority_key = format!("1.3.000{n}");
                row
            })
            .collect();
        apply(&mut state, vec![daemon("default", vec![focused.clone()])]);
        let slots = state.slots.clone();
        let lights = device_output(&state);

        let report = report_of(&mut focused);
        report.agents[7].agent.title = Some("renamed".into());
        report.agents[0]
            .agent
            .tokens
            .push(("task".into(), "x".into()));
        report.agents.swap(1, 2);
        report.active_endpoint_id = Some("ssh:0123456789abcdef0123456789abcdef".into());
        apply(&mut state, vec![daemon("default", vec![focused.clone()])]);
        assert_eq!(slots, state.slots);
        assert_eq!(lights, device_output(&state));

        // A slotted agent changing status relights without moving.
        let slotted = report_of(&mut focused)
            .agents
            .iter_mut()
            .find(|row| row.agent.pane_id == "p0")
            .unwrap();
        slotted.agent.agent_status = "done".into();
        apply(&mut state, vec![daemon("default", vec![focused])]);
        assert_eq!(slots, state.slots);
        assert_ne!(lights, device_output(&state));
    }

    #[test]
    fn slots_wait_for_their_client_and_another_client_starts_empty() {
        let mut state = State::new(Config::default());
        let mut focused = client(7, Some(true));
        apply(&mut state, vec![daemon("default", vec![focused.clone()])]);
        let slots = state.slots.clone();
        let lights = device_output(&state);

        // The daemon is lost for a moment: the keypad is dark, and when its
        // client is followed again, its agents in another priority order,
        // every key means the same.
        let mut lost = daemon("default", vec![]);
        lost.state = DaemonState::Offline;
        apply(&mut state, vec![lost]);
        assert_eq!(device_output(&state).lighting, Default::default());
        let report = focused.report.as_mut().unwrap();
        report.agents[3].agent.priority_key = "0.0.0000".into();
        apply(&mut state, vec![daemon("default", vec![focused.clone()])]);
        assert_eq!(state.slots, slots);
        assert_eq!(device_output(&state).lighting.slots, lights.lighting.slots);

        // Another client's keys start from its own report.
        let mut other = focused.clone();
        other.client_id = 8;
        apply(&mut state, vec![daemon("default", vec![focused, other])]);
        assert_ne!(state.slots, slots);
        assert_eq!(
            state.slots[0].as_ref().map(|key| key.pane_id.as_str()),
            Some("w2:p2")
        );
    }

    #[test]
    fn an_official_device_owner_or_a_stopping_bridge_disables_routing() {
        let mut state = State::new(Config::default());
        let focused = vec![daemon("default", vec![client(7, Some(true))])];
        state.owner = Some(ExternalOwner::Input);
        apply(&mut state, focused.clone());
        assert!(!state.routing_ready);
        assert_eq!(state.status()["routing"], "unavailable");
        let context = input_context(&state);
        assert!(context.client.is_none());
        assert!(context.slots.iter().all(Option::is_none));

        state.owner = None;
        let directory = state.directory.updated(focused);
        apply_clients_update(&mut state, directory, &AtomicBool::new(true));
        assert!(!state.routing_ready);
    }
}
