//! Bridge state and its reconciliation with Herdr sessions, terminal
//! mappings, macOS frontmost/ownership, and the HID device.

use anyhow::Result;
use herdr_client::AgentInfo;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::Sender,
    },
    time::{Duration, Instant},
};

use crate::{
    actions::{GHOSTTY_PROCESS, automatic_layer, layer_identity},
    config::{Config, Controls, enabled_buttons},
    device::DeviceEvent,
    ghostty::{SessionTerminalMapping, focused_terminal_id, probe_session_terminals},
    herdr::{Session, SessionUpdate, SessionWorker, discover_sessions, spawn_session_worker},
    hid::{HidClient, connection_error_state},
    macos,
    protocol::{
        INPUT_BUNDLE_ID, SLOT_COUNT, aggregate_lighting, assign_slots, device_owner, slot_lighting,
    },
};

use super::{DAEMON_PROTOCOL_VERSION, dispatch::agent_identity, log, log_changed};

const MAPPING_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const DEVICE_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const NO_SESSIONS_SHUTDOWN: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct State {
    pub(super) config: Config,
    pub(super) sessions: Vec<Session>,
    pub(super) mappings: Vec<SessionTerminalMapping>,
    selected: Option<Selected>,
    pub(super) routing_verified: bool,
    pub(super) routing_ready: bool,
    pub(super) routing_generation: Arc<AtomicU64>,
    pub(super) agents: Vec<AgentInfo>,
    pub(super) slots: Vec<Option<String>>,
    pub(super) device_state: String,
    pub(super) owner: Option<String>,
    pub(super) frontmost: Option<macos::Frontmost>,
    pub(super) focused_terminal: Option<String>,
    pub(super) active_layer: Option<usize>,
    pub(super) last_device_error: String,
    pub(super) last_herdr_error: String,
    pub(super) last_frontmost_error: String,
    pub(super) last_controls_error: String,
    pub(super) last_routing_error: String,
    pub(super) last_lighting_error: String,
    pub(super) last_lighting: String,
    pub(super) last_sent_layer: Option<usize>,
    pub(super) managed_aggregate_zones: HashSet<String>,
    pub(super) no_sessions_at: Option<Instant>,
    pub(super) next_mapping_probe: Option<Instant>,
    pub(super) next_device_open: Option<Instant>,
    pub(super) device_restore_pending: bool,
}

impl State {
    pub(super) fn new(config: Config) -> Self {
        Self {
            config,
            slots: vec![None; SLOT_COUNT],
            device_state: "starting".into(),
            ..Self::default()
        }
    }

    pub(super) fn status(&self) -> Value {
        json!({
            "device": if self.owner.is_some() { "yielded" } else { &self.device_state },
            "deviceError": (!self.last_device_error.is_empty()).then_some(&self.last_device_error),
            "owner": self.owner,
            "session": self.selected.as_ref().map(|selected| &selected.session),
            "routing": if self.routing_ready { "ready" } else if self.selected.is_some() { "unavailable" } else { "none" },
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": DAEMON_PROTOCOL_VERSION,
            "sessions": self.sessions.iter().map(|session| &session.name).collect::<Vec<_>>(),
            "sessionMappings": self.mappings.iter().map(|mapping| json!({
                "session": mapping.session_name,
                "terminal": mapping.terminal_id,
            })).collect::<Vec<_>>(),
            "layer": self.active_layer,
            "agents": self.agents.len(),
            "frontmost": self.frontmost.as_ref().map(|frontmost| json!({
                "appName": frontmost.app_name,
                "process": frontmost.process,
            })),
            "focusedTerminal": self.focused_terminal,
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
        self.routing_verified = false;
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

struct Selected {
    session: String,
    generation: u64,
    pending_agents: Option<Vec<AgentInfo>>,
    pending_slots: Vec<Option<String>>,
}

#[derive(Clone)]
pub(super) struct InputRoute {
    pub(super) session: Session,
    pub(super) terminal: String,
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

fn device_failure(state: &mut State, context: &str, error: String) {
    state.device_state = "helper-error".into();
    log_changed(&mut state.last_device_error, error, &format!("{context}: "));
}

fn device_disconnected(state: &mut State, error: String) {
    device_failure(state, "device disconnected", error);
    state.device_restore_pending = true;
    state.next_device_open = Some(Instant::now());
}

pub(super) fn send_lighting(device: &HidClient, state: &mut State) -> Result<()> {
    let slots_value: Vec<_> = slot_lighting(&state.slots, &state.agents, &state.config.lighting)
        .into_iter()
        .enumerate()
        .map(|(id, light)| json!({"id":id,"c":light.c,"b":light.b,"e":light.e,"s":light.s}))
        .collect();
    let mut aggregate = aggregate_lighting(&state.slots, &state.agents, &state.config.lighting);
    let next_zones: HashSet<_> = aggregate.keys().cloned().collect();
    for zone in &state.managed_aggregate_zones {
        if !next_zones.contains(zone) {
            aggregate.insert(zone.clone(), crate::config::Light::default());
        }
    }
    let aggregate_value: serde_json::Map<String, Value> = aggregate
        .into_iter()
        .map(|(zone, light)| {
            (
                zone,
                json!({"c":light.c,"b":light.b,"e":light.e,"s":light.s}),
            )
        })
        .collect();
    let signature = json!({"slots": &slots_value, "aggregate": &aggregate_value}).to_string();
    if signature == state.last_lighting {
        return Ok(());
    }
    if !aggregate_value.is_empty() {
        device.send("v.oai.rgbcfg", Some(Value::Object(aggregate_value)))?;
    }
    device.send("v.oai.thstatus", Some(Value::Array(slots_value)))?;
    state.managed_aggregate_zones = next_zones;
    state.last_lighting = signature;
    Ok(())
}

pub(super) fn close_device(device: &mut Option<HidClient>, state: &mut State, blank: bool) {
    state.last_lighting.clear();
    let restore_was_pending = state.device_restore_pending;
    let Some(mut device) = device.take() else {
        return;
    };
    if blank {
        if !state.managed_aggregate_zones.is_empty() {
            let zones: serde_json::Map<String, Value> = state
                .managed_aggregate_zones
                .iter()
                .map(|zone| (zone.clone(), json!({"e":0,"b":0,"s":0,"c":0})))
                .collect();
            let _ = device.send("v.oai.rgbcfg", Some(Value::Object(zones)));
        }
        let blank = (0..SLOT_COUNT)
            .map(|id| json!({"id":id,"c":0,"b":0,"e":0,"s":0}))
            .collect();
        let _ = device.send("v.oai.thstatus", Some(Value::Array(blank)));
    }
    state.last_sent_layer = None;
    if let Err(error) = select_layer(Some(&device), state, Some(1)) {
        log(format!("safe layer selection failed: {error:#}"));
    }
    state.managed_aggregate_zones.clear();
    match device.close() {
        Ok(()) if !restore_was_pending => {
            state.device_restore_pending = false;
            state.last_device_error.clear();
        }
        Ok(()) => {}
        Err(error) => {
            state.device_restore_pending = true;
            state.next_device_open = Some(Instant::now());
            device_failure(state, "device close failed", error.to_string());
        }
    }
}

fn select_layer(device: Option<&HidClient>, state: &mut State, layer: Option<usize>) -> Result<()> {
    let Some(layer) = layer else { return Ok(()) };
    state.active_layer = Some(layer);
    let Some(device) = device else { return Ok(()) };
    if state.last_sent_layer != Some(layer) {
        device.send(
            "host.focused_app",
            Some(serde_json::to_value(layer_identity(layer))?),
        )?;
        state.last_sent_layer = Some(layer);
        log(format!("layer {layer} selected"));
    }
    Ok(())
}

fn select_session(
    device: Option<&HidClient>,
    state: &mut State,
    next: Option<String>,
) -> Result<()> {
    if state.selected.as_ref().map(|selected| &selected.session) == next.as_ref() {
        return Ok(());
    }
    reset_selected_route(state);
    state.selected = next.map(|session| Selected {
        session,
        generation: 0,
        pending_agents: None,
        pending_slots: Vec::new(),
    });
    if let Some(device) = device {
        send_lighting(device, state)?;
    }
    log(state
        .selected
        .as_ref()
        .map(|selected| format!("Herdr session selected: {}", selected.session))
        .unwrap_or_else(|| "Herdr session unselected".into()));
    Ok(())
}

fn reset_selected_route(state: &mut State) {
    state.revoke_routing();
    state.agents.clear();
    state.slots.fill(None);
    state.last_lighting.clear();
}

fn routing_failure(state: &mut State, message: impl Into<String>) {
    log_changed(
        &mut state.last_routing_error,
        message.into(),
        "Herdr session routing failed: ",
    );
}

pub(super) fn refresh_sessions(
    state: &mut State,
    device: Option<&HidClient>,
    workers: &mut HashMap<String, SessionWorker>,
    worker_generation: &mut u64,
    updates: &Sender<SessionUpdate>,
    stopping: &Arc<AtomicBool>,
) -> Result<bool> {
    let discovered = match discover_sessions() {
        Ok(sessions) => sessions,
        Err(error) => {
            state.revoke_routing();
            state.mappings.clear();
            routing_failure(state, error.to_string());
            return Err(error);
        }
    };
    let now = Instant::now();
    if discovered.is_empty() {
        state.no_sessions_at.get_or_insert(now);
    } else {
        state.no_sessions_at = None;
    }
    let changed = discovered != state.sessions;
    let selected = state
        .selected
        .as_ref()
        .map(|selected| selected.session.as_str());
    let previous_selected =
        selected.and_then(|name| state.sessions.iter().find(|session| session.name == name));
    let discovered_selected =
        selected.and_then(|name| discovered.iter().find(|session| session.name == name));
    let selected_changed = previous_selected != discovered_selected;
    if selected_changed {
        workers.clear();
        if let Some(selected) = &mut state.selected {
            selected.generation = 0;
            selected.pending_agents = None;
            selected.pending_slots.clear();
        }
        if previous_selected.is_some() && discovered_selected.is_some() {
            reset_selected_route(state);
            if let Some(device) = device {
                send_lighting(device, state)?;
            }
        }
    }
    state.sessions = discovered;
    if state.selected.as_ref().is_some_and(|selected| {
        !state
            .sessions
            .iter()
            .any(|session| session.name == selected.session)
    }) {
        select_session(device, state, None)?;
    }
    sync_selected_worker(state, workers, worker_generation, updates, stopping);
    if changed {
        state.mappings.clear();
        refresh_mappings(state);
    }
    Ok(state
        .no_sessions_at
        .is_some_and(|at| now.duration_since(at) >= NO_SESSIONS_SHUTDOWN))
}

pub(super) fn sync_selected_worker(
    state: &mut State,
    workers: &mut HashMap<String, SessionWorker>,
    worker_generation: &mut u64,
    updates: &Sender<SessionUpdate>,
    stopping: &Arc<AtomicBool>,
) {
    workers.retain(|name, _| {
        state
            .selected
            .as_ref()
            .is_some_and(|selected| selected.session == *name)
    });
    let Some(session) = state.selected.as_ref().and_then(|selected| {
        state
            .sessions
            .iter()
            .find(|session| session.name == selected.session)
    }) else {
        return;
    };
    if workers.contains_key(&session.name) {
        return;
    }
    let session = session.clone();
    *worker_generation += 1;
    if let Some(selected) = &mut state.selected {
        selected.generation = *worker_generation;
    }
    workers.insert(
        session.name.clone(),
        spawn_session_worker(
            session,
            *worker_generation,
            updates.clone(),
            Arc::clone(stopping),
        ),
    );
}

pub(super) fn refresh_mappings(state: &mut State) {
    state.next_mapping_probe = None;
    if state.sessions.is_empty() {
        state.revoke_routing();
        state.mappings.clear();
        return;
    }
    match probe_session_terminals(&state.sessions) {
        Ok(found) => {
            let changed = state.mappings != found;
            if changed {
                state.revoke_routing();
                state.mappings = found;
            }
            state.next_mapping_probe = Some(Instant::now() + MAPPING_REFRESH_INTERVAL);
            state.last_routing_error.clear();
            if changed {
                log(format!(
                    "Herdr sessions mapped: {}",
                    state
                        .mappings
                        .iter()
                        .map(|mapping| format!("{}={}", mapping.session_name, mapping.terminal_id))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        Err(error) => {
            state.revoke_routing();
            state.mappings.clear();
            state.next_mapping_probe = Some(Instant::now() + MAPPING_REFRESH_INTERVAL);
            routing_failure(state, error.to_string());
        }
    }
}

pub(super) fn refresh_routing(state: &mut State, device: Option<&HidClient>) -> Result<bool> {
    let Some(frontmost_process) = state
        .frontmost
        .as_ref()
        .map(|frontmost| frontmost.process.clone())
    else {
        state.revoke_routing();
        return Ok(false);
    };
    if frontmost_process != GHOSTTY_PROCESS {
        let changed = state.focused_terminal.take().is_some();
        let previous_layer = state.active_layer;
        state.next_mapping_probe = None;
        state.revoke_routing();
        select_layer(
            device,
            state,
            automatic_layer(Some(&frontmost_process), None),
        )?;
        return Ok(changed || previous_layer != state.active_layer);
    }
    match focused_terminal_id() {
        Ok(terminal) => {
            let terminal_changed = state.focused_terminal.as_deref() != Some(&terminal);
            let previous_layer = state.active_layer;
            state.focused_terminal = Some(terminal.clone());
            let mut session = state
                .mappings
                .iter()
                .find(|mapping| mapping.terminal_id == terminal)
                .map(|mapping| mapping.session_name.clone());
            if terminal_changed && session.is_none() {
                refresh_mappings(state);
                session = state
                    .mappings
                    .iter()
                    .find(|mapping| mapping.terminal_id == terminal)
                    .map(|mapping| mapping.session_name.clone());
            }
            let Some(session) =
                session.filter(|name| state.sessions.iter().any(|session| session.name == *name))
            else {
                state.next_mapping_probe.get_or_insert(Instant::now());
                state.revoke_routing();
                return Ok(terminal_changed);
            };
            select_session(device, state, Some(session.clone()))?;
            select_layer(
                device,
                state,
                automatic_layer(Some(&frontmost_process), Some(&session)),
            )?;
            state.routing_verified = true;
            state.last_routing_error.clear();
            apply_selected_agents(state, device)?;
            return Ok(terminal_changed || previous_layer != state.active_layer);
        }
        Err(error) => {
            state.focused_terminal = None;
            routing_failure(state, error.to_string());
            state.revoke_routing();
        }
    }
    Ok(false)
}

fn apply_selected_agents(state: &mut State, device: Option<&HidClient>) -> Result<()> {
    let Some(selected) = &state.selected else {
        return Ok(());
    };
    let (available, agents) = match &selected.pending_agents {
        Some(agents) => (true, (*agents != state.agents).then(|| agents.clone())),
        None => (false, None),
    };
    if available && agents.is_none() {
        state.routing_ready = true;
        state.last_herdr_error.clear();
        return Ok(());
    }
    match agents {
        Some(agents) => {
            let pending_slots = state
                .selected
                .as_ref()
                .expect("selected session")
                .pending_slots
                .clone();
            if agent_identity(state.agents.iter().find(|agent| agent.focused))
                != agent_identity(agents.iter().find(|agent| agent.focused))
            {
                state.invalidate_routing();
            }
            let slots = assign_slots(&pending_slots, &agents);
            state
                .selected
                .as_mut()
                .expect("selected session")
                .pending_slots = slots.clone();
            state.agents = agents;
            state.slots = slots;
            state.routing_ready = true;
            state.last_herdr_error.clear();
            if let Some(device) = device {
                send_lighting(device, state)?;
            }
        }
        None => {
            let had_state = state.routing_ready
                || !state.agents.is_empty()
                || state.slots.iter().any(Option::is_some);
            state.invalidate_routing();
            state.agents.clear();
            state.slots.fill(None);
            if had_state {
                state.last_lighting.clear();
                if let Some(device) = device {
                    send_lighting(device, state)?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn apply_session_update(
    state: &mut State,
    update: SessionUpdate,
    workers: &HashMap<String, SessionWorker>,
    device: Option<&HidClient>,
) -> Result<bool> {
    let (session, generation) = match &update {
        SessionUpdate::Agents {
            session,
            generation,
            ..
        }
        | SessionUpdate::Unavailable {
            session,
            generation,
            ..
        } => (session, *generation),
    };
    if workers.get(session).map(|worker| worker.generation) != Some(generation)
        || !state.selected.as_ref().is_some_and(|selected| {
            selected.session == *session && selected.generation == generation
        })
    {
        return Ok(false);
    }
    match update {
        SessionUpdate::Agents {
            session, agents, ..
        } => {
            state
                .selected
                .as_mut()
                .expect("validated selected session")
                .pending_agents = Some(agents);
            if session_route_is_verified(state, &session) {
                state.last_herdr_error.clear();
                apply_selected_agents(state, device)?;
            }
            Ok(false)
        }
        SessionUpdate::Unavailable { session, error, .. } => {
            let became_unavailable = state
                .selected
                .as_mut()
                .expect("validated selected session")
                .pending_agents
                .take()
                .is_some();
            apply_selected_agents(state, device)?;
            log_changed(
                &mut state.last_herdr_error,
                error,
                &format!("Herdr session {session} unavailable: "),
            );
            Ok(became_unavailable)
        }
    }
}

fn session_route_is_verified(state: &State, session: &str) -> bool {
    state.routing_verified
        && state
            .selected
            .as_ref()
            .is_some_and(|selected| selected.session == session)
}

pub(super) fn refresh_frontmost(state: &mut State) -> bool {
    match macos::frontmost_pid() {
        Ok(pid) if state.frontmost.as_ref().is_some_and(|app| app.pid == pid) => return false,
        Ok(_) => {}
        Err(error) => {
            let changed = state.frontmost.take().is_some();
            state.revoke_routing();
            log_changed(
                &mut state.last_frontmost_error,
                error.to_string(),
                "frontmost window unavailable: ",
            );
            return changed;
        }
    }
    match macos::frontmost() {
        Ok(frontmost) => {
            let changed = state.frontmost.as_ref() != Some(&frontmost);
            state.frontmost = Some(frontmost);
            state.last_frontmost_error.clear();
            changed
        }
        Err(error) if state.last_frontmost_error != error.to_string() => {
            let changed = state.frontmost.take().is_some();
            state.revoke_routing();
            log_changed(
                &mut state.last_frontmost_error,
                error.to_string(),
                "frontmost window unavailable: ",
            );
            changed
        }
        Err(_) => {
            let changed = state.frontmost.take().is_some();
            state.revoke_routing();
            changed
        }
    }
}

pub(super) fn refresh_owner(state: &mut State, device: &mut Option<HidClient>) -> bool {
    let frontmost = state
        .frontmost
        .as_ref()
        .map(|frontmost| frontmost.process.clone());
    let owner = device_owner(
        macos::bundle_is_running(INPUT_BUNDLE_ID),
        frontmost.as_deref(),
    )
    .map(str::to_owned);
    if owner != state.owner {
        state.next_mapping_probe = owner.is_none().then(Instant::now);
        state.owner = owner;
        if let Some(owner) = &state.owner {
            log(format!("yielding to {owner}"));
            state.revoke_routing();
            close_device(device, state, false);
        } else {
            log("device owner cleared");
            state.next_device_open = None;
        }
        true
    } else {
        false
    }
}

pub(super) fn open_device(
    device: &mut Option<HidClient>,
    event_tx: &Sender<DeviceEvent>,
    state: &mut State,
) -> Result<bool> {
    if device.is_some()
        || (state.owner.is_some() && !state.device_restore_pending)
        || state
            .next_device_open
            .is_some_and(|deadline| Instant::now() < deadline)
    {
        return Ok(false);
    }
    state.next_device_open = None;
    match HidClient::connect(event_tx.clone()) {
        Ok(mut opened) if state.owner.is_some() => {
            state.last_sent_layer = None;
            if let Err(error) = select_layer(Some(&opened), state, Some(1)) {
                log(format!(
                    "safe layer selection during recovery failed: {error:#}"
                ));
            }
            match opened.close() {
                Ok(()) => {
                    state.device_restore_pending = false;
                    state.last_device_error.clear();
                    log("native HID ownership recovered");
                }
                Err(error) => {
                    state.next_device_open = Some(Instant::now() + DEVICE_RETRY_INTERVAL);
                    device_failure(state, "native HID recovery failed", error.to_string());
                }
            }
        }
        Ok(opened) => {
            *device = Some(opened);
            state.device_state = "connected".into();
            state.device_restore_pending = false;
            state.last_device_error.clear();
            state.last_lighting.clear();
            log("device connected");
            select_layer(device.as_ref(), state, state.active_layer)?;
            if let Some(device) = device.as_ref() {
                send_lighting(device, state)?;
            }
        }
        Err(error) => {
            state.next_device_open = Some(Instant::now() + DEVICE_RETRY_INTERVAL);
            let message = error.to_string();
            state.device_state = connection_error_state(&error).into();
            log_changed(
                &mut state.last_device_error,
                message,
                "device open failed: ",
            );
        }
    }
    Ok(true)
}

pub(super) fn publish(status: &Arc<Mutex<Value>>, state: &State) {
    *status.lock().unwrap_or_else(|error| error.into_inner()) = state.status();
}

pub(super) fn update_input_context(context: &Mutex<Arc<InputContext>>, state: &State) {
    let target = state.agents.iter().find(|agent| agent.focused);
    let route = state.routing_ready.then(|| {
        let selected = state.selected.as_ref()?;
        let session = state
            .sessions
            .iter()
            .find(|session| session.name == selected.session)?;
        Some(InputRoute {
            session: session.clone(),
            terminal: state.focused_terminal.clone()?,
            generation: state.routing_generation(),
        })
    });
    *context.lock().unwrap_or_else(|error| error.into_inner()) = Arc::new(InputContext {
        controls: state.config.controls.clone(),
        route: route.flatten(),
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

pub(super) fn handle_input_disconnect(
    state: &mut State,
    device: &mut Option<HidClient>,
    error: String,
) -> bool {
    if device.is_none() {
        return false;
    }
    device_disconnected(state, error);
    state.revoke_routing();
    close_device(device, state, false);
    true
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
                state.last_lighting.clear();
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
    use std::sync::mpsc;

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

    fn session(name: &str) -> Session {
        Session {
            name: name.into(),
            socket_path: format!("/tmp/{name}.sock").into(),
        }
    }
    #[test]
    fn status_matches_the_control_contract() {
        let mut state = State::new(Config::default());
        state.selected = Some(Selected {
            session: "work".into(),
            generation: 1,
            pending_agents: None,
            pending_slots: Vec::new(),
        });
        state.routing_ready = true;
        state.sessions = vec![Session {
            name: "work".into(),
            socket_path: "/tmp/herdr.sock".into(),
        }];
        state.mappings = vec![SessionTerminalMapping {
            session_name: "work".into(),
            terminal_id: "t1".into(),
        }];
        let status = state.status();
        assert!(status["deviceError"].is_null());
        state.last_device_error = "USB restoration failed".into();
        let status = state.status();
        assert_eq!(status["routing"], "ready");
        assert_eq!(
            status["sessionMappings"],
            json!([{"session":"work","terminal":"t1"}])
        );
        assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(status["protocol"], DAEMON_PROTOCOL_VERSION);
        assert_eq!(status["deviceError"], "USB restoration failed");
        assert_eq!(status.as_object().unwrap().len(), 14);
    }

    #[test]
    fn unexpected_disconnect_requests_immediate_native_restore() {
        let mut state = State::new(Config::default());
        let before = Instant::now();
        device_disconnected(&mut state, "helper died".into());
        assert!(state.device_restore_pending);
        assert!(
            state
                .next_device_open
                .is_some_and(|deadline| deadline >= before)
        );
        assert_eq!(state.last_device_error, "helper died");
    }
    #[test]
    fn revoking_routing_invalidates_queued_work() {
        let mut state = State::new(Config::default());
        state.routing_ready = true;
        let queued = state.routing_generation();
        state.revoke_routing();
        assert_ne!(queued, state.routing_generation());
    }

    #[test]
    fn resetting_selected_route_revokes_published_identities() {
        let mut state = State::new(Config::default());
        state.selected = Some(Selected {
            session: "work".into(),
            generation: 1,
            pending_agents: None,
            pending_slots: Vec::new(),
        });
        state.sessions = vec![session("work")];
        state.routing_verified = true;
        state.routing_ready = true;
        state.agents = vec![agent("terminal", "pane", "codex")];
        state.slots[0] = Some("terminal".into());
        let queued = state.routing_generation();
        let replacement = Session {
            name: "work".into(),
            socket_path: "/tmp/replacement.sock".into(),
        };

        let previous = state.sessions.iter().find(|session| session.name == "work");
        let discovered = [replacement];
        let replacement = discovered.iter().find(|session| session.name == "work");
        assert_ne!(previous, replacement);
        if previous.is_some() && replacement.is_some() {
            reset_selected_route(&mut state);
        }

        assert!(!state.routing_verified);
        assert!(!state.routing_ready);
        assert_ne!(queued, state.routing_generation());
        assert!(state.agents.is_empty());
        assert!(state.slots.iter().all(Option::is_none));
    }

    #[test]
    fn selected_session_requires_a_live_snapshot() {
        let mut state = State::new(Config::default());
        state.selected = Some(Selected {
            session: "work".into(),
            generation: 1,
            pending_agents: Some(vec![agent("terminal", "pane", "codex")]),
            pending_slots: Vec::new(),
        });

        apply_selected_agents(&mut state, None).unwrap();
        assert!(state.routing_ready);
        state.selected.as_mut().unwrap().pending_agents = None;
        apply_selected_agents(&mut state, None).unwrap();
        assert!(!state.routing_ready);
        assert!(state.agents.is_empty());
    }

    #[test]
    fn agent_updates_require_a_verified_terminal_route() {
        let mut state = State::new(Config::default());
        state.selected = Some(Selected {
            session: "work".into(),
            generation: 7,
            pending_agents: None,
            pending_slots: Vec::new(),
        });
        state.focused_terminal = Some("terminal".into());
        state.frontmost = Some(macos::Frontmost {
            app_name: "Ghostty".into(),
            process: GHOSTTY_PROCESS.into(),
            pid: 1,
        });
        let stopping = Arc::new(AtomicBool::new(true));
        let (updates, _) = mpsc::channel();
        let worker = spawn_session_worker(session("work"), 7, updates, stopping);
        let workers = HashMap::from([("work".into(), worker)]);
        let update = || SessionUpdate::Agents {
            session: "work".into(),
            generation: 7,
            agents: vec![agent("terminal", "pane", "codex")],
        };

        apply_session_update(&mut state, update(), &workers, None).unwrap();
        assert!(!state.routing_ready);

        state.routing_verified = true;
        apply_session_update(&mut state, update(), &workers, None).unwrap();
        assert!(state.routing_ready);
    }

    #[test]
    fn agent_updates_reject_wrong_sessions_and_stale_workers() {
        let mut state = State::new(Config::default());
        state.selected = Some(Selected {
            session: "work".into(),
            generation: 7,
            pending_agents: None,
            pending_slots: Vec::new(),
        });
        let stopping = Arc::new(AtomicBool::new(true));
        let (updates, _) = mpsc::channel();
        let workers = HashMap::from([
            (
                "work".into(),
                spawn_session_worker(session("work"), 7, updates.clone(), Arc::clone(&stopping)),
            ),
            (
                "other".into(),
                spawn_session_worker(session("other"), 7, updates, stopping),
            ),
        ]);
        let update = |session: &str| SessionUpdate::Agents {
            session: session.into(),
            generation: 7,
            agents: Vec::new(),
        };

        assert!(!apply_session_update(&mut state, update("other"), &workers, None).unwrap());
        state.selected.as_mut().unwrap().generation = 6;
        assert!(!apply_session_update(&mut state, update("work"), &workers, None).unwrap());
        assert!(state.selected.unwrap().pending_agents.is_none());
    }

    #[test]
    fn live_session_unavailability_requests_one_discovery() {
        let mut state = State::new(Config::default());
        state.selected = Some(Selected {
            session: "work".into(),
            generation: 7,
            pending_agents: Some(Vec::new()),
            pending_slots: Vec::new(),
        });
        let stopping = Arc::new(AtomicBool::new(true));
        let (updates, _) = mpsc::channel();
        let worker = spawn_session_worker(session("work"), 7, updates, Arc::clone(&stopping));
        let workers = HashMap::from([("work".into(), worker)]);
        let unavailable = || SessionUpdate::Unavailable {
            session: "work".into(),
            generation: 7,
            error: "closed".into(),
        };

        assert!(apply_session_update(&mut state, unavailable(), &workers, None).unwrap());
        assert!(!apply_session_update(&mut state, unavailable(), &workers, None).unwrap());
    }

    #[test]
    fn owner_refresh_reports_only_real_transitions() {
        let mut state = State::new(Config::default());
        state.frontmost = Some(macos::Frontmost {
            app_name: "ChatGPT".into(),
            process: crate::actions::CHATGPT_BUNDLE_IDS[0].into(),
            pid: 1,
        });
        let mut device = None;
        state.next_mapping_probe = Some(Instant::now() - Duration::from_secs(1));

        assert!(refresh_owner(&mut state, &mut device));
        assert!(state.next_mapping_probe.is_none());
        assert!(!refresh_owner(&mut state, &mut device));
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
