//! Bridge state and its reconciliation with the hub's active session and the
//! HID device.

use codex_micro::{
    ExternalOwner, external_owner,
    service::{Lighting, ServiceStatus},
};
use herdr_hub_client::{AgentInfo, HubClient, Model, ServerMessage, SessionState};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use crate::{
    actions::HERDR_LAYER,
    config::{Config, Controls, enabled_buttons},
    hub::{self, Session},
    protocol::{SLOT_COUNT, aggregate_lighting, assign_slots, slot_lighting},
};

use super::{DAEMON_PROTOCOL_VERSION, device, dispatch::agent_identity, log, log_changed};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ActiveRoute {
    pub(super) session_key: Option<String>,
}

pub(super) struct State {
    pub(super) config: Config,
    pub(super) model: Model,
    pub(super) active_route: Arc<Mutex<ActiveRoute>>,
    pub(super) routing_ready: bool,
    pub(super) routing_generation: Arc<AtomicU64>,
    pub(super) agents: Vec<AgentInfo>,
    pub(super) slots: Vec<Option<String>>,
    pub(super) device_state: String,
    pub(super) owner: Option<ExternalOwner>,
    pub(super) active_layer: Option<usize>,
    pub(super) last_device_error: String,
    pub(super) last_herdr_error: String,
    pub(super) last_controls_error: String,
}

impl State {
    pub(super) fn new(config: Config) -> Self {
        Self {
            config,
            model: empty_model(),
            active_route: Arc::new(Mutex::new(ActiveRoute::default())),
            routing_ready: false,
            routing_generation: Arc::new(AtomicU64::new(0)),
            agents: Vec::new(),
            slots: vec![None; SLOT_COUNT],
            device_state: "starting".into(),
            owner: None,
            active_layer: None,
            last_device_error: String::new(),
            last_herdr_error: String::new(),
            last_controls_error: String::new(),
        }
    }

    pub(super) fn status(&self) -> Value {
        let active = active_session(&self.model);
        json!({
            "device": if self.owner.is_some() { "yielded" } else { &self.device_state },
            "deviceError": (!self.last_device_error.is_empty()).then_some(&self.last_device_error),
            "owner": self.owner.map(|owner| owner.to_string()),
            "session": active.map(|session| &session.name),
            "routing": if self.routing_ready { "ready" } else if self.model.active.is_some() { "unavailable" } else { "none" },
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": DAEMON_PROTOCOL_VERSION,
            "sessions": self.model.sessions.iter().filter(|session| session.connected).map(|session| &session.name).collect::<Vec<_>>(),
            "layer": self.active_layer,
            "agents": self.agents.len(),
            "slots": self.slots.iter().map(|id| id.as_ref().and_then(|id| {
                self.agents.iter().find(|agent| agent.terminal_id == *id).map(|agent| json!({
                    "pane": agent.pane_id,
                    "agent": agent.agent,
                    "status": agent.agent_status,
                }))
            })).collect::<Vec<_>>(),
        })
    }

    pub(super) fn revoke_routing(&mut self) {
        self.invalidate_routing();
    }

    pub(super) fn invalidate_routing(&mut self) {
        if self.routing_ready {
            self.routing_ready = false;
            self.routing_generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(super) fn routing_generation(&self) -> u64 {
        self.routing_generation.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub(super) struct InputRoute {
    pub(super) session: Session,
    pub(super) generation: u64,
}

#[derive(Clone)]
pub(super) struct InputContext {
    pub(super) controls: Controls,
    pub(super) route: Option<InputRoute>,
    pub(super) target: Option<AgentInfo>,
    pub(super) slots: Vec<Option<AgentInfo>>,
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

    pub(super) fn selected(&self, routing_generation: &AtomicU64) -> Option<InputRoute> {
        self.route
            .as_ref()
            .filter(|route| route.generation == routing_generation.load(Ordering::Acquire))
            .cloned()
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
        lighting: Lighting {
            slots: slot_lighting(&state.slots, &state.agents, &state.config.lighting),
            aggregate: aggregate_lighting(&state.slots, &state.agents, &state.config.lighting)
                .into_iter()
                .collect(),
        },
    }
}

fn empty_model() -> Model {
    Model {
        version: 0,
        active: None,
        hosts: Vec::new(),
        sessions: Vec::new(),
    }
}

fn active_session(model: &Model) -> Option<&SessionState> {
    let key = model.active.as_deref()?;
    model
        .sessions
        .iter()
        .find(|session| session.key == key && session.connected)
}

fn active_route(model: &Model) -> ActiveRoute {
    ActiveRoute {
        session_key: active_session(model).map(|session| session.key.clone()),
    }
}

fn routing_identity(model: &Model) -> (ActiveRoute, Option<(String, String, Option<String>)>) {
    let route = active_route(model);
    let focused =
        active_session(model).and_then(|session| session.agents.iter().find(|agent| agent.focused));
    (route, agent_identity(focused))
}

#[cfg(test)]
fn sync_active_route(state: &State) {
    *state
        .active_route
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = active_route(&state.model);
}

pub(super) fn reconcile_active(state: &mut State, stopping: &AtomicBool) {
    let Some(session) = active_session(&state.model) else {
        state.revoke_routing();
        state.agents.clear();
        state.slots.fill(None);
        return;
    };

    state.slots = assign_slots(&state.slots, &session.agents);
    state.agents.clone_from(&session.agents);
    if state.owner.is_some() || stopping.load(Ordering::Acquire) {
        state.revoke_routing();
    } else {
        state.active_layer = Some(HERDR_LAYER);
        state.routing_ready = true;
    }
}

pub(super) fn apply_hub_update(state: &mut State, update: hub::Update, stopping: &AtomicBool) {
    let published_route = Arc::clone(&state.active_route);
    let mut published_route = published_route
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let previous_identity = routing_identity(&state.model);
    let connected = update.is_ok();
    match update {
        Ok(ServerMessage::Hello { model, .. }) => state.model = model,
        Ok(message) => HubClient::apply(&mut state.model, &message),
        Err(error) => {
            state.model = empty_model();
            log_changed(
                &mut state.last_herdr_error,
                error.to_string(),
                "Herdr hub unavailable: ",
            );
        }
    }
    let next_identity = routing_identity(&state.model);
    if previous_identity != next_identity {
        state.invalidate_routing();
    }
    published_route.clone_from(&next_identity.0);
    drop(published_route);
    if previous_identity.0.session_key != next_identity.0.session_key {
        log(next_identity
            .0
            .session_key
            .as_ref()
            .map(|key| format!("Herdr session selected: {key}"))
            .unwrap_or_else(|| "Herdr session unselected".into()));
    }
    if connected {
        state.last_herdr_error.clear();
    }
    reconcile_active(state, stopping)
}

pub(super) fn refresh_owner(state: &mut State) -> bool {
    let owner = external_owner();
    if owner != state.owner {
        state.owner = owner;
        if let Some(owner) = state.owner {
            log(format!("device owned by {owner}"));
            state.revoke_routing();
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
    let target = state.agents.iter().find(|agent| agent.focused);
    let route = state
        .routing_ready
        .then(|| active_session(&state.model))
        .flatten()
        .map(|session| InputRoute {
            session: Session {
                key: session.key.clone(),
                name: session.name.clone(),
                socket_path: session.socket_path.clone(),
            },
            generation: state.routing_generation(),
        });
    *context.lock().unwrap_or_else(|error| error.into_inner()) = Arc::new(InputContext {
        controls: state.config.controls.clone(),
        route,
        target: target.cloned(),
        slots: state
            .slots
            .iter()
            .map(|id| {
                id.as_deref()
                    .and_then(|id| state.agents.iter().find(|agent| agent.terminal_id == id))
                    .cloned()
            })
            .collect(),
    });
}

pub(super) fn handle_input_disconnect(state: &mut State, error: String) {
    state.device_state = "unavailable".into();
    log_changed(&mut state.last_device_error, error, "device disconnected: ");
    state.revoke_routing();
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
    use herdr_hub_client::{Error as HubError, HostState, SessionState};

    use super::*;

    fn agent(terminal: &str, pane: &str, kind: &str) -> AgentInfo {
        serde_json::from_value(json!({
            "terminal_id": terminal,
            "agent": kind,
            "agent_status": "idle",
            "workspace_id": "w1",
            "tab_id": "w1:t1",
            "pane_id": pane,
            "focused": true,
            "state_change_seq": 1,
            "cwd": "/tmp",
            "revision": 1
        }))
        .unwrap()
    }

    fn session(key: &str, agents: Vec<AgentInfo>) -> SessionState {
        SessionState {
            key: key.into(),
            host: "local".into(),
            name: key.rsplit('/').next().unwrap().into(),
            connected: true,
            error: None,
            protocol: 20,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            agents,
            socket_path: Some(format!("/tmp/{}.sock", key.rsplit('/').next().unwrap()).into()),
            client_focused: Some(true),
        }
    }

    fn model(agents: Vec<AgentInfo>) -> Model {
        Model {
            version: 4,
            active: Some("local/work".into()),
            hosts: vec![HostState {
                key: "local".into(),
                connected: true,
                error: None,
            }],
            sessions: vec![session("local/work", agents)],
        }
    }

    fn hello(model: Model) -> hub::Update {
        Ok(ServerMessage::Hello {
            protocol: herdr_hub_client::PROTOCOL,
            model,
        })
    }

    #[test]
    fn hub_model_active_session_supplies_agents_and_route() {
        let mut state = State::new(Config::default());
        let stopping = AtomicBool::new(false);
        let mut active = model(vec![agent("terminal", "pane", "codex")]);
        active.sessions[0].client_focused = None;
        apply_hub_update(&mut state, hello(active), &stopping);
        assert!(state.routing_ready);
        assert_eq!(state.agents.len(), 1);
        assert_eq!(state.slots[0].as_deref(), Some("terminal"));
        let shared = Mutex::new(Arc::new(InputContext::new(&state.config)));
        update_input_context(&shared, &state);
        let context = Arc::clone(&shared.lock().unwrap());
        let route = context.route.as_ref().unwrap();
        assert_eq!(route.session.key, "local/work");
        assert_eq!(
            *state.active_route.lock().unwrap(),
            ActiveRoute {
                session_key: Some("local/work".into()),
            }
        );
    }

    #[test]
    fn title_churn_updates_context_without_changing_lights_or_routing() {
        let stopping = AtomicBool::new(false);
        let mut state = State::new(Config::default());
        let mut current = agent("terminal", "pane", "codex");
        apply_hub_update(&mut state, hello(model(vec![current.clone()])), &stopping);
        let output = device_output(&state);
        let generation = state.routing_generation();
        let context = Mutex::new(Arc::new(InputContext::new(&state.config)));

        for revision in 2..102 {
            current.terminal_title = Some(format!("working {revision}"));
            current.revision = revision;
            apply_hub_update(
                &mut state,
                Ok(ServerMessage::Session {
                    version: revision,
                    session: session("local/work", vec![current.clone()]),
                }),
                &stopping,
            );
            update_input_context(&context, &state);
            assert_eq!(device_output(&state), output);
            assert_eq!(state.routing_generation(), generation);
            assert_eq!(
                context
                    .lock()
                    .unwrap()
                    .target
                    .as_ref()
                    .unwrap()
                    .terminal_title,
                current.terminal_title
            );
        }

        current.agent_status = "working".into();
        apply_hub_update(&mut state, hello(model(vec![current])), &stopping);
        assert_ne!(device_output(&state), output);
    }

    #[test]
    fn active_and_focused_agent_changes_invalidate_captured_routes() {
        let stopping = AtomicBool::new(false);
        let mut initial = model(vec![agent("work-agent", "work-pane", "codex")]);
        initial.sessions.push(session(
            "local/personal",
            vec![agent("personal-agent", "personal-pane", "claude")],
        ));
        let mut state = State::new(Config::default());
        apply_hub_update(&mut state, hello(initial), &stopping);
        let first_generation = state.routing_generation();

        apply_hub_update(
            &mut state,
            Ok(ServerMessage::Active {
                version: 5,
                key: Some("local/personal".into()),
            }),
            &stopping,
        );
        assert!(state.routing_ready);
        assert_ne!(state.routing_generation(), first_generation);
        assert_eq!(state.agents[0].pane_id, "personal-pane");

        let agent_generation = state.routing_generation();
        apply_hub_update(
            &mut state,
            Ok(ServerMessage::Session {
                version: 7,
                session: session(
                    "local/personal",
                    vec![agent("new-agent", "new-pane", "claude")],
                ),
            }),
            &stopping,
        );
        assert_ne!(state.routing_generation(), agent_generation);
        assert_eq!(state.agents[0].pane_id, "new-pane");
    }

    #[test]
    fn disconnected_or_missing_active_session_clears_the_route() {
        let stopping = AtomicBool::new(false);
        let mut state = State::new(Config::default());
        apply_hub_update(
            &mut state,
            hello(model(vec![agent("terminal", "pane", "codex")])),
            &stopping,
        );
        let ready_generation = state.routing_generation();

        let mut disconnected = session("local/work", vec![agent("terminal", "pane", "codex")]);
        disconnected.connected = false;
        apply_hub_update(
            &mut state,
            Ok(ServerMessage::Session {
                version: 5,
                session: disconnected,
            }),
            &stopping,
        );
        assert!(!state.routing_ready);
        assert!(state.agents.is_empty());
        assert!(state.slots.iter().all(Option::is_none));
        assert_ne!(state.routing_generation(), ready_generation);

        apply_hub_update(
            &mut state,
            Ok(ServerMessage::Session {
                version: 6,
                session: session("local/work", vec![agent("terminal", "pane", "codex")]),
            }),
            &stopping,
        );
        assert!(state.routing_ready);
        let restored_generation = state.routing_generation();

        apply_hub_update(
            &mut state,
            Ok(ServerMessage::Active {
                version: 7,
                key: None,
            }),
            &stopping,
        );
        assert!(!state.routing_ready);
        assert!(state.agents.is_empty());
        assert_ne!(state.routing_generation(), restored_generation);
        assert_eq!(*state.active_route.lock().unwrap(), ActiveRoute::default());
    }

    #[test]
    fn hub_outage_revokes_routes_and_blanks_agents() {
        let mut state = State::new(Config::default());
        let stopping = AtomicBool::new(false);
        apply_hub_update(
            &mut state,
            hello(model(vec![agent("terminal", "pane", "codex")])),
            &stopping,
        );
        let generation = state.routing_generation();

        apply_hub_update(&mut state, Err(HubError::Disconnected), &stopping);

        assert!(state.model.sessions.is_empty());
        assert!(state.model.active.is_none());
        assert!(!state.routing_ready);
        assert!(state.agents.is_empty());
        assert!(state.slots.iter().all(Option::is_none));
        assert_ne!(state.routing_generation(), generation);
        assert_eq!(state.last_herdr_error, "hub disconnected");
    }

    #[test]
    fn status_matches_the_control_contract() {
        let mut state = State::new(Config::default());
        let stopping = AtomicBool::new(false);
        apply_hub_update(&mut state, hello(model(Vec::new())), &stopping);
        let status = state.status();
        assert_eq!(status["session"], "work");
        assert_eq!(status["routing"], "ready");
        assert!(status.get("sessionMappings").is_none());
        assert!(status.get("focusedTerminal").is_none());
        assert!(status.get("frontmost").is_none());
        assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(status["protocol"], DAEMON_PROTOCOL_VERSION);
    }

    #[test]
    fn owner_gate_revokes_then_restores_routing() {
        let stopping = AtomicBool::new(false);
        let mut state = State::new(Config::default());
        state.model = model(vec![agent("terminal", "pane", "codex")]);
        sync_active_route(&state);
        state.owner = Some(ExternalOwner::Input);
        reconcile_active(&mut state, &stopping);
        assert!(!state.routing_ready);
        state.owner = None;
        reconcile_active(&mut state, &stopping);
        assert!(state.routing_ready);
    }

    #[test]
    fn service_status_replaces_stale_device_availability() {
        let mut state = State::new(Config::default());
        state.device_state = "connected".into();
        let mut status = ServiceStatus {
            device: None,
            external_owner: None,
            last_error: Some("reopen failed".into()),
            input_monitoring: codex_micro::InputMonitoringAccess::Granted,
        };

        assert!(apply_service_status(&mut state, &status));
        assert_eq!(state.device_state, "unavailable");
        status.device = Some(codex_micro::DeviceInfo {
            transport: codex_micro::Transport::Usb,
            firmware: "0.6.2".into(),
        });
        status.last_error = None;
        assert!(apply_service_status(&mut state, &status));
        assert_eq!(state.device_state, "connected");
    }

    #[test]
    fn config_load_reports_only_config_changes() {
        let config = Config::default();
        let enabled = enabled_buttons(&config.controls);
        let mut state = State::new(config.clone());

        assert!(!apply_config_load(&mut state, Ok(config.clone()), enabled));
        assert!(!apply_config_load(
            &mut state,
            Err("invalid config".into()),
            enabled
        ));
        assert!(!apply_config_load(
            &mut state,
            Err("invalid config".into()),
            enabled
        ));
        assert!(!apply_config_load(&mut state, Ok(config.clone()), enabled));
        let mut changed = config;
        changed.lighting.focused_brightness /= 2.0;
        assert!(apply_config_load(&mut state, Ok(changed), enabled));
    }
}
