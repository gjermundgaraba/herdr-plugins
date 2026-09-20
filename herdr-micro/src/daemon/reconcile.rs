//! Bridge state and its reconciliation with the uniquely focused TUI and the
//! HID device.

use crate::frontends::{ClientState, Model};
use codex_micro::{ExternalOwner, external_owner, service::ServiceStatus};
use herdr_client::frontend::Agent;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use crate::{
    actions::HERDR_LAYER,
    config::{Config, Controls, enabled_buttons},
    frontends::{self, ClientRoute},
    protocol::{SLOT_COUNT, SlotKey, assign_slots, lighting},
};

use super::{DAEMON_PROTOCOL_VERSION, device, log, log_changed};

pub(super) struct State {
    pub(super) config: Config,
    pub(super) model: Model,
    pub(super) routing_ready: bool,
    pub(super) agents: Vec<(SlotKey, Agent)>,
    pub(super) available: Vec<(SlotKey, ClientRoute, Agent)>,
    pub(super) slots: Vec<Option<SlotKey>>,
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
            model: empty_model(),
            routing_ready: false,
            agents: Vec::new(),
            available: Vec::new(),
            slots: vec![None; SLOT_COUNT],
            device_state: "starting".into(),
            owner: None,
            active_layer: None,
            last_device_error: String::new(),
            last_controls_error: String::new(),
        }
    }

    pub(super) fn status(&self) -> Value {
        let active = self.model.foremost_client();
        json!({
            "device": if self.owner.is_some() { "yielded" } else { &self.device_state },
            "deviceError": (!self.last_device_error.is_empty()).then_some(&self.last_device_error),
            "owner": self.owner.map(|owner| owner.to_string()),
            "client": active.map(|client| &client.client_id),
            "routing": if self.routing_ready { "ready" } else if self.model.foremost_client().is_some() { "unavailable" } else { "none" },
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": DAEMON_PROTOCOL_VERSION,
            "endpoints": active.map(|client| client.endpoints.iter().map(|endpoint| json!({
                "id": endpoint.endpoint_id, "label": endpoint.label, "status": endpoint.status,
            })).collect::<Vec<_>>()),
            "layer": self.active_layer,
            "agents": self.agents.len(),
            "inputTarget": active.and_then(|client| client.input_target.as_ref()),
            "slots": self.slots.iter().map(|slot| slot.as_ref().and_then(|slot| {
                self.available.iter().find(|(key, _, _)| key == slot).map(|(_, route, agent)| json!({
                    "endpoint": route.endpoint_id,
                    "pane": agent.pane_id,
                    "agent": agent.agent,
                    "status": agent.agent_status,
                }))
            })).collect::<Vec<_>>(),
        })
    }

    pub(super) fn disable_routing(&mut self) {
        self.routing_ready = false;
    }
}

#[derive(Clone)]
pub(super) struct InputContext {
    pub(super) controls: Controls,
    pub(super) route: Option<ClientRoute>,
    pub(super) target: Option<Agent>,
    pub(super) slots: Vec<Option<(ClientRoute, Agent)>>,
}

impl InputContext {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            controls: config.controls.clone(),
            route: None,
            target: None,
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
/// compares this value, so title, token, and revision changes never resend LEDs.
pub(super) fn device_output(state: &State) -> device::Output {
    device::Output {
        layer: state.active_layer,
        lighting: if state.model.foremost_client().is_none() {
            Default::default()
        } else {
            lighting(&state.slots, &state.agents, &state.config.lighting)
        },
    }
}

fn empty_model() -> Model {
    Model::default()
}

fn selected_agent(client: &ClientState) -> Option<Agent> {
    let target = client.input_target.as_ref()?;
    let endpoint = client
        .endpoints
        .iter()
        .find(|e| e.endpoint_id == target.endpoint_id && e.is_available())?;
    let snapshot = endpoint.snapshot.as_ref()?;
    snapshot
        .agents
        .iter()
        .find(|a| a.pane_id == target.pane_id)
        .cloned()
}

pub(super) fn reconcile_active(state: &mut State, stopping: &AtomicBool) {
    state.available.clear();
    if let Some(client) = state.model.foremost_client() {
        for endpoint in &client.endpoints {
            let Some(route) = client.route(&endpoint.endpoint_id) else {
                continue;
            };
            let Some(snapshot) = &endpoint.snapshot else {
                continue;
            };
            for agent in &snapshot.agents {
                let key = (endpoint.endpoint_id.clone(), agent.pane_id.clone());
                state.available.push((key, route.clone(), agent.clone()));
            }
        }
    }
    state.agents = state
        .available
        .iter()
        .map(|(key, route, agent)| {
            let mut projected = agent.clone();
            projected.focused = agent.focused
                && state.model.foremost_client().is_some_and(|c| {
                    c.active_endpoint_id.as_deref() == Some(route.endpoint_id.as_str())
                });
            (key.clone(), projected)
        })
        .collect();
    state.slots = assign_slots(&state.slots, &state.agents);
    if state.model.foremost_client().is_none()
        || state.owner.is_some()
        || stopping.load(Ordering::Acquire)
    {
        state.disable_routing();
    } else {
        state.active_layer = Some(HERDR_LAYER);
        state.routing_ready = true;
    }
}

pub(super) fn apply_frontend_update(
    state: &mut State,
    clients: frontends::Update,
    stopping: &AtomicBool,
) {
    let before = state.model.foremost_client().map(|c| c.client_id.clone());
    state.model.clients = clients;
    let after = state.model.foremost_client().map(|c| c.client_id.clone());
    if before != after {
        log(after
            .map(|id| format!("Herdr client selected: {id}"))
            .unwrap_or_else(|| "Herdr client unselected".into()));
    }
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

pub(super) fn update_input_context(context: &Mutex<Arc<InputContext>>, state: &State) {
    let client = state.model.foremost_client();
    let target = client.and_then(selected_agent);
    let route = state
        .routing_ready
        .then(|| client.map(ClientState::input_route))
        .flatten();
    *context.lock().unwrap_or_else(|error| error.into_inner()) = Arc::new(InputContext {
        controls: state.config.controls.clone(),
        route,
        target,
        slots: state
            .slots
            .iter()
            .map(|slot| {
                state
                    .routing_ready
                    .then(|| {
                        slot.as_ref()
                            .and_then(|slot| state.available.iter().find(|(key, _, _)| key == slot))
                            .map(|(_, route, agent)| (route.clone(), agent.clone()))
                    })
                    .flatten()
            })
            .collect(),
    });
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
mod tests {
    use super::*;
    use herdr_client::frontend::Snapshot;
    fn client(id: &str, focused: Option<bool>) -> ClientState {
        ClientState { socket_path: format!("/tmp/{id}.sock").into(), snapshot: serde_json::from_value::<Snapshot>(json!({
            "client_id":id,"pid":1,"revision":1,"focused":focused,"input_ready":true,"active_endpoint_id":"a","input_target":null,
            "endpoints":[endpoint("a"),endpoint("b")]
        })).unwrap() }
    }
    fn endpoint(id: &str) -> Value {
        json!({"endpoint_id":id,"label":id,"status":"online","generation":1,"boot_id":"boot","methods":[],
        "snapshot":{"boot_id":"boot","revision":1,"workspaces":[],"tabs":[],"panes":[],"agents":[{"pane_id":"pane","workspace_id":"w","tab_id":"t","agent":"codex","agent_status":"idle","state_change_seq":1,"state_labels":[],"tokens":[],"focused":true}]}})
    }
    fn apply(state: &mut State, clients: Vec<ClientState>) {
        apply_frontend_update(state, clients, &AtomicBool::new(false));
    }
    #[test]
    fn only_unique_true_focus_routes_and_lights() {
        let mut state = State::new(Config::default());
        for clients in [
            vec![],
            vec![client("a", None)],
            vec![client("a", Some(false))],
            vec![client("a", Some(true)), client("b", Some(true))],
        ] {
            apply(&mut state, clients);
            assert!(!state.routing_ready);
            assert!(state.agents.is_empty());
            assert!(state.slots.iter().all(Option::is_none));
        }
        apply(&mut state, vec![client("a", Some(true)), client("b", None)]);
        assert!(state.routing_ready);
        assert_eq!(state.agents.len(), 2);
        assert_ne!(state.slots[0], state.slots[1]);
        assert!(state.agents[0].1.focused);
        assert!(!state.agents[1].1.focused);
    }
    #[test]
    fn overlay_or_unavailable_lease_preserves_ordinary_input_route() {
        let mut state = State::new(Config::default());
        let mut c = client("a", Some(true));
        c.snapshot.input_ready = false;
        apply(&mut state, vec![c]);
        let shared = Mutex::new(Arc::new(InputContext::new(&state.config)));
        update_input_context(&shared, &state);
        let context = shared.into_inner().unwrap();
        assert!(context.route.is_some());
        assert!(context.target.is_none());
    }
    #[test]
    fn sticky_pane_slots_survive_endpoint_switch_and_agent_title_churn() {
        let mut state = State::new(Config::default());
        let mut c = client("a", Some(true));
        let s = c.snapshot.endpoints[0].snapshot.as_mut().unwrap();
        let template = s.agents[0].clone();
        s.agents = (0..8)
            .map(|n| {
                let mut a = template.clone();
                a.pane_id = format!("p{n}");
                a.state_change_seq = n;
                a
            })
            .collect();
        apply(&mut state, vec![c.clone()]);
        let slots = state.slots.clone();
        let lights = device_output(&state);
        c.snapshot.revision += 1;
        c.snapshot.endpoints[0].snapshot.as_mut().unwrap().agents[7].title = Some("renamed".into());
        apply(&mut state, vec![c.clone()]);
        assert_eq!(slots, state.slots);
        assert_eq!(lights, device_output(&state));
        c.snapshot.active_endpoint_id = Some("b".into());
        apply(&mut state, vec![c.clone()]);
        assert_eq!(slots, state.slots);
        c.snapshot.endpoints[0].snapshot.as_mut().unwrap().agents[7].agent_status = "done".into();
        apply(&mut state, vec![c.clone()]);
        let lights = device_output(&state);
        // Acknowledgment-only status snapshot must relight without a new runtime revision.
        c.snapshot.endpoints[0].snapshot.as_mut().unwrap().agents[7].agent_status = "idle".into();
        apply(&mut state, vec![c]);
        assert_ne!(lights, device_output(&state));
    }
}
