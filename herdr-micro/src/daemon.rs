//! The long-lived Codex Micro bridge.  Keep the policy here; HID framing,
//! Herdr parsing, gestures, and macOS operations live in their small modules.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use signal_hook::consts::{SIGINT, SIGTERM};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    actions::{
        automatic_layer, diff_pane_args, execute_effort_plan, fast_mode_plan, layer_identity,
        parse_agents, plan_effort_change, prompt_args, scroll_plan, submit_args, Agent,
    },
    config::{
        config_path, key_binding, load_controls, load_effort, load_lighting, provision_controls,
        provision_effort, provision_lighting, Action, AgentStatus, Binding, Controls, Direction,
        EffortDirection, VerticalDirection,
    },
    control::listen_for_control,
    device::{DeviceEvent, MicroDevice},
    gestures::{Fired, GestureDispatcher},
    ghostty::{
        focused_session, inspect_ghostty, probe_session_terminals, GhosttyState,
        SessionTerminalMapping,
    },
    herdr::{
        current_environment, discover_sessions, herdr_bin, run_command, run_json,
        session_environment, Environment,
    },
    macos,
    protocol::{
        aggregate_lighting, assign_slots, device_owner, joystick_event, slot_lighting, SLOT_COUNT,
    },
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const NO_SESSIONS_SHUTDOWN: Duration = Duration::from_secs(60);
const MAPPING_REPROBE: Duration = Duration::from_secs(30);
const WORK_QUEUE_CAPACITY: usize = 16;
pub const DAEMON_PROTOCOL_VERSION: u32 = 1;

fn runtime_controls(
    active_hid_keys: &BTreeMap<u8, Option<String>>,
    mut next: Controls,
) -> (Controls, bool) {
    let pending = next.hid_keys != *active_hid_keys;
    if pending {
        next.hid_keys.clone_from(active_hid_keys);
    }
    (next, pending)
}

fn log(message: impl AsRef<str>) {
    eprintln!(
        "{} {}",
        format_timestamp(SystemTime::now()),
        message.as_ref()
    );
}

fn format_timestamp(time: SystemTime) -> String {
    let elapsed = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = elapsed.as_secs() as libc::time_t;
    let mut utc: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `seconds` and `utc` are valid writable C time values for this call.
    if unsafe { libc::gmtime_r(&seconds, &mut utc) }.is_null() {
        return "1970-01-01T00:00:00.000Z".into();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        utc.tm_year + 1900,
        utc.tm_mon + 1,
        utc.tm_mday,
        utc.tm_hour,
        utc.tm_min,
        utc.tm_sec,
        elapsed.subsec_millis(),
    )
}

#[derive(Default)]
struct State {
    sessions: Vec<String>,
    mappings: Vec<SessionTerminalMapping>,
    selected_session: Option<String>,
    routing_ready: bool,
    routing_generation: Arc<AtomicU64>,
    session_slots: HashMap<String, Vec<Option<String>>>,
    agents: Vec<Agent>,
    slots: Vec<Option<String>>,
    device_state: String,
    owner: Option<String>,
    frontmost: Option<macos::Frontmost>,
    ghostty: Option<GhosttyState>,
    active_layer: Option<usize>,
    last_open_error: String,
    last_herdr_error: String,
    last_frontmost_error: String,
    last_controls_error: String,
    last_lighting_error: String,
    last_routing_error: String,
    last_lighting: String,
    last_focused_app: String,
    managed_aggregate_zones: HashSet<String>,
    no_sessions_at: Option<Instant>,
    next_mapping_probe: Option<Instant>,
    last_joystick_sector: Option<u8>,
}

impl State {
    fn new() -> Self {
        Self {
            slots: vec![None; SLOT_COUNT],
            device_state: "starting".into(),
            ..Self::default()
        }
    }

    fn status(&self) -> Value {
        json!({
            "device": if self.owner.is_some() { "yielded" } else { &self.device_state },
            "owner": self.owner,
            "session": self.selected_session,
            "routing": if self.routing_ready { "ready" } else if self.selected_session.is_some() { "unavailable" } else { "none" },
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": DAEMON_PROTOCOL_VERSION,
            "sessions": self.sessions,
            "sessionMappings": self.mappings.iter().map(|mapping| json!({
                "session": mapping.session_name,
                "terminal": mapping.terminal_id,
            })).collect::<Vec<_>>(),
            "layer": self.active_layer,
            "agents": self.agents.len(),
            "frontmost": self.frontmost.as_ref().map(|frontmost| json!({
                "appName": frontmost.app_name,
                "process": frontmost.process,
                "title": frontmost.title,
            })),
            "focusedTerminal": self.ghostty.as_ref().and_then(|state| state.focused_terminal_id.as_ref()),
            "slots": self.slots.iter().map(|id| id.as_ref().and_then(|id| {
                self.agents.iter().find(|agent| agent.terminal_id == *id).map(|agent| json!({
                    "pane": agent.pane_id,
                    "agent": agent.agent,
                    "status": agent.agent_status,
                }))
            })).collect::<Vec<_>>(),
        })
    }

    fn selected(&self) -> Option<String> {
        self.routing_ready
            .then(|| self.selected_session.clone())
            .flatten()
    }

    fn revoke_routing(&mut self) {
        if self.routing_ready {
            self.routing_ready = false;
            self.routing_generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn routing_generation(&self) -> u64 {
        self.routing_generation.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
enum Work {
    Binding {
        binding: Box<Binding>,
        source: String,
        session: String,
        target: Option<Agent>,
        generation: u64,
    },
    FocusSlot {
        pane_id: String,
        source: String,
        session: String,
        generation: u64,
    },
}

impl Work {
    fn generation(&self) -> u64 {
        match self {
            Self::Binding { generation, .. } | Self::FocusSlot { generation, .. } => *generation,
        }
    }
}

fn list_agents(session: &str, base: &Environment) -> Result<Vec<Agent>> {
    let args = vec!["agent".into(), "list".into()];
    parse_agents(&run_json(
        &herdr_bin(),
        &args,
        Some(&session_environment(session, base)),
    )?)
}

fn require_agent(agent: Option<&Agent>) -> Result<&Agent> {
    agent.ok_or_else(|| anyhow!("no focused Herdr agent"))
}

fn agent_identity(agent: Option<&Agent>) -> Option<(&str, &str, &str)> {
    agent.map(|agent| {
        (
            agent.terminal_id.as_str(),
            agent.pane_id.as_str(),
            agent.agent.as_str(),
        )
    })
}

fn ready_agent(agent: Option<&Agent>) -> Result<&Agent> {
    let agent = require_agent(agent)?;
    if matches!(agent.agent_status, AgentStatus::Idle | AgentStatus::Done) {
        Ok(agent)
    } else {
        bail!("focused agent is {}", agent_status_name(agent.agent_status))
    }
}

fn agent_status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
    }
}

fn action_name(action: &Action) -> &'static str {
    match action {
        Action::Prompt { .. } => "prompt",
        Action::Diff => "diff",
        Action::Fast => "fast",
        Action::Submit => "submit",
        Action::Effort { .. } => "effort",
        Action::FocusPane { .. } => "focus-pane",
        Action::Scroll { .. } => "scroll",
    }
}

fn run_herdr(args: Vec<String>, session: &str, base: &Environment) -> Result<Value> {
    run_json(
        &herdr_bin(),
        &args,
        Some(&session_environment(session, base)),
    )
}

fn execute_scroll(
    direction: VerticalDirection,
    percent: f64,
    expected_pane: &str,
    session: &str,
    base: &Environment,
    mappings: &Arc<Mutex<Vec<SessionTerminalMapping>>>,
) -> Result<bool> {
    if !session_is_frontmost(session, mappings)? {
        log(format!("scroll ignored: {session} is not frontmost"));
        return Ok(false);
    }
    let pane = run_herdr(vec!["pane".into(), "current".into()], session, base)?;
    if pane.pointer("/result/pane/pane_id").and_then(Value::as_str) != Some(expected_pane) {
        log(format!("scroll ignored: focused pane changed in {session}"));
        return Ok(false);
    }
    let layout = run_herdr(vec!["pane".into(), "layout".into()], session, base)?;
    let plan = scroll_plan(
        pane.pointer("/result/pane").unwrap_or(&Value::Null),
        layout.pointer("/result/layout").unwrap_or(&Value::Null),
        match direction {
            VerticalDirection::Up => "up",
            VerticalDirection::Down => "down",
        },
        percent,
    )?;
    if !session_is_frontmost(session, mappings)? {
        log(format!("scroll ignored: {session} is no longer frontmost"));
        return Ok(false);
    }
    let frontmost = macos::frontmost()?;
    macos::scroll(
        i32::try_from(plan.notches).map_err(|_| anyhow!("scroll distance is too large"))?,
        plan.x,
        plan.y,
        &frontmost.process,
    )?;
    log(format!(
        "scrolled {} {percent}% in {session}/{}",
        match direction {
            VerticalDirection::Up => "up",
            VerticalDirection::Down => "down",
        },
        plan.pane_id
    ));
    Ok(true)
}

fn session_is_frontmost(
    session: &str,
    mappings: &Arc<Mutex<Vec<SessionTerminalMapping>>>,
) -> Result<bool> {
    if macos::frontmost()?.process != crate::actions::GHOSTTY_PROCESS {
        return Ok(false);
    }
    let mappings = mappings
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    Ok(focused_session(&mappings, &inspect_ghostty()?).as_deref() == Some(session))
}

fn execute_action(
    action: &Action,
    current: Option<&Agent>,
    session: &str,
    base: &Environment,
    mappings: &Arc<Mutex<Vec<SessionTerminalMapping>>>,
) -> Result<bool> {
    match action {
        Action::Prompt { prompt, submit } => {
            let args = prompt_args(prompt, submit.unwrap_or(true), ready_agent(current)?)?;
            run_herdr(args, session, base)?;
        }
        Action::Diff => {
            run_herdr(diff_pane_args(require_agent(current)?)?, session, base)?;
        }
        Action::Fast => {
            for args in fast_mode_plan(ready_agent(current)?)? {
                run_herdr(args, session, base)?;
            }
        }
        Action::Submit => {
            run_herdr(submit_args(require_agent(current)?)?, session, base)?;
        }
        Action::Effort { direction } => {
            let current = require_agent(current)?;
            let effort =
                load_effort(&config_path("effort.json")).map_err(|error| anyhow!(error))?;
            let direction = match direction {
                EffortDirection::Raise => "raise",
                EffortDirection::Lower => "lower",
            };
            let plan = plan_effort_change(&current.agent, direction, &current.pane_id, &effort)?;
            execute_effort_plan(&herdr_bin(), &plan, |bin, args| {
                run_json(bin, args, Some(&session_environment(session, base))).map(|_| ())
            })?;
        }
        Action::FocusPane { direction } => {
            let direction = match direction {
                Direction::Up => "up",
                Direction::Down => "down",
                Direction::Left => "left",
                Direction::Right => "right",
            };
            let pane = require_agent(current)?.pane_id.clone();
            run_herdr(
                ["pane", "focus", "--direction", direction, "--pane", &pane]
                    .map(str::to_owned)
                    .into(),
                session,
                base,
            )?;
            log(format!("joystick focus {direction}: {session}/{pane}"));
        }
        Action::Scroll { direction, percent } => {
            return execute_scroll(
                *direction,
                *percent,
                &require_agent(current)?.pane_id,
                session,
                base,
                mappings,
            )
        }
    }
    Ok(true)
}

fn action_worker(
    receiver: Receiver<Work>,
    base: Environment,
    mappings: Arc<Mutex<Vec<SessionTerminalMapping>>>,
    routing_generation: Arc<AtomicU64>,
    stopping: Arc<AtomicBool>,
) {
    while !stopping.load(Ordering::Acquire) {
        let Ok(work) = receiver.recv_timeout(Duration::from_millis(50)) else {
            continue;
        };
        let generation = work.generation();
        if generation != routing_generation.load(Ordering::Acquire) {
            log("control ignored: stale Herdr routing");
            continue;
        }
        let session = match &work {
            Work::Binding { session, .. } | Work::FocusSlot { session, .. } => session,
        };
        match session_is_frontmost(session, &mappings) {
            Ok(true) => {}
            Ok(false) => {
                log("control ignored: Herdr session is no longer frontmost");
                continue;
            }
            Err(error) => {
                log(format!(
                    "control ignored: could not verify Herdr routing: {error}"
                ));
                continue;
            }
        }
        if generation != routing_generation.load(Ordering::Acquire) {
            log("control ignored: stale Herdr routing");
            continue;
        }
        let result = match work {
            Work::FocusSlot {
                pane_id,
                source,
                session,
                ..
            } => {
                let result = run_herdr(
                    vec!["agent".into(), "focus".into(), pane_id.clone()],
                    &session,
                    &base,
                );
                if result.is_ok() {
                    log(format!("{source}: focused {session}/{pane_id}"));
                }
                result.map(|_| ())
            }
            Work::Binding {
                binding,
                source,
                session,
                target,
                ..
            } => {
                // This lookup belongs in the serialized worker: it is the last
                // possible moment before delivery, not the physical event time.
                let result = (|| {
                    let agents = list_agents(&session, &base)?;
                    if generation != routing_generation.load(Ordering::Acquire) {
                        log(format!("{source} ignored: stale Herdr routing"));
                        return Ok(());
                    }
                    let current = agents.iter().find(|agent| agent.focused);
                    if agent_identity(current) != agent_identity(target.as_ref()) {
                        log(format!("{source} ignored: focused pane changed"));
                        return Ok(());
                    }
                    let Some(action) =
                        binding.resolve(current.map(|agent| agent.agent.as_str()).unwrap_or(""))
                    else {
                        return Ok(());
                    };
                    if execute_action(&action, current, &session, &base, &mappings)? {
                        log(format!(
                            "{source}: {} in {session}{}",
                            action_name(&action),
                            current
                                .map(|agent| format!(" for {} in {}", agent.agent, agent.pane_id))
                                .unwrap_or_default()
                        ));
                    }
                    Ok(())
                })();
                result
            }
        };
        if let Err(error) = result {
            log(format!("control failed: {error:#}"));
        }
    }
}

fn queue_binding(
    sender: &SyncSender<Work>,
    binding: Binding,
    source: String,
    session: Option<String>,
    target: Option<Agent>,
    generation: u64,
) {
    match session {
        Some(session) => {
            let work = Work::Binding {
                binding: Box::new(binding),
                source,
                session,
                target,
                generation,
            };
            if let Err(error) = sender.try_send(work) {
                log(match error {
                    TrySendError::Full(_) => "control ignored: action queue full",
                    TrySendError::Disconnected(_) => "control ignored: worker stopped",
                });
            }
        }
        None => log(format!("{source} ignored: no ready Herdr session")),
    }
}

fn handle_fired(
    sender: &SyncSender<Work>,
    target: Option<Agent>,
    generation: u64,
    fired: Vec<Fired>,
) {
    for fired in fired {
        queue_binding(
            sender,
            Binding::Action(fired.action),
            fired.source,
            fired.context,
            target.clone(),
            generation,
        );
    }
}

fn handle_device_event(
    event: DeviceEvent,
    state: &mut State,
    controls: &Controls,
    gestures: &mut GestureDispatcher,
    worker: &SyncSender<Work>,
) -> bool {
    match event {
        DeviceEvent::Disconnected => {
            log("device disconnected");
            true
        }
        DeviceEvent::Joystick { angle, distance } => {
            let next = joystick_event(
                angle,
                distance,
                state.last_joystick_sector,
                controls.joystick.engage_distance,
                controls.joystick.release_distance,
            );
            state.last_joystick_sector = next.sector;
            let action = match next.direction {
                Some("up") => controls.joystick.up.clone(),
                Some("down") => controls.joystick.down.clone(),
                Some("left") => controls.joystick.left.clone(),
                Some("right") => controls.joystick.right.clone(),
                _ => None,
            };
            if let Some(action) = action {
                queue_binding(
                    worker,
                    Binding::Action(action),
                    format!("joystick {}", next.direction.unwrap()),
                    state.selected(),
                    state.agents.iter().find(|agent| agent.focused).cloned(),
                    state.routing_generation(),
                );
            }
            false
        }
        DeviceEvent::Key { key, action } => {
            if let Some(index) = key
                .strip_prefix("AG0")
                .and_then(|index| index.parse::<usize>().ok())
                .filter(|index| *index < SLOT_COUNT)
            {
                if action == 1 {
                    let session = state.selected();
                    let agent = state.slots[index]
                        .as_ref()
                        .and_then(|id| state.agents.iter().find(|agent| &agent.terminal_id == id));
                    match (session, agent) {
                        (Some(session), Some(agent)) => {
                            let work = Work::FocusSlot {
                                pane_id: agent.pane_id.clone(),
                                source: key,
                                session,
                                generation: state.routing_generation(),
                            };
                            if let Err(error) = worker.try_send(work) {
                                log(match error {
                                    TrySendError::Full(_) => "control ignored: action queue full",
                                    TrySendError::Disconnected(_) => {
                                        "control ignored: worker stopped"
                                    }
                                });
                            }
                        }
                        _ => log(format!("{key} ignored: no ready Agent slot")),
                    }
                }
                return false;
            }
            let binding = key_binding(controls, &key, action);
            let captured = state.selected();
            if matches!(key.as_str(), "ENC_CC" | "ENC_CW") {
                if let Some(binding) = binding {
                    queue_binding(
                        worker,
                        binding,
                        key,
                        captured,
                        state.agents.iter().find(|agent| agent.focused).cloned(),
                        state.routing_generation(),
                    );
                }
            } else if matches!(action, 0 | 1) {
                match binding.as_ref() {
                    Some(Binding::Gesture(_)) => handle_fired(
                        worker,
                        state.agents.iter().find(|agent| agent.focused).cloned(),
                        state.routing_generation(),
                        gestures.handle(
                            key,
                            binding.as_ref(),
                            action == 1,
                            captured,
                            Instant::now(),
                        ),
                    ),
                    Some(_) if action == 1 => queue_binding(
                        worker,
                        binding.unwrap(),
                        key,
                        captured,
                        state.agents.iter().find(|agent| agent.focused).cloned(),
                        state.routing_generation(),
                    ),
                    _ => {}
                }
            }
            false
        }
    }
}

fn send_lighting(device: &MicroDevice, state: &mut State) -> Result<()> {
    let config = match load_lighting(&config_path("lighting.json")) {
        Ok(config) => {
            state.last_lighting_error.clear();
            config
        }
        Err(error) => {
            if state.last_lighting_error != error {
                state.last_lighting_error = error.clone();
                log(format!("lighting configuration failed: {error}"));
            }
            return Ok(());
        }
    };
    let slots_value: Vec<_> = slot_lighting(&state.slots, &state.agents, &config)
        .into_iter()
        .enumerate()
        .map(|(id, light)| json!({"id":id,"c":light.c,"b":light.b,"e":light.e,"s":light.s}))
        .collect();
    let mut aggregate = aggregate_lighting(&state.slots, &state.agents, &config);
    let next_zones: HashSet<_> = aggregate.keys().cloned().collect();
    for zone in &state.managed_aggregate_zones {
        if !next_zones.contains(zone) {
            aggregate.insert(
                zone.clone(),
                crate::config::Light {
                    c: 0,
                    b: 0.0,
                    e: 0,
                    s: 0.0,
                },
            );
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
    let lighting = json!({"slots": slots_value, "aggregate": aggregate_value});
    let signature = lighting.to_string();
    if signature == state.last_lighting {
        return Ok(());
    }
    let aggregate = lighting
        .get("aggregate")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if aggregate.as_object().is_some_and(|zones| !zones.is_empty()) {
        device.send("v.oai.rgbcfg", Some(aggregate))?;
    }
    device.send("v.oai.thstatus", lighting.get("slots").cloned())?;
    state.managed_aggregate_zones = next_zones;
    state.last_lighting = signature;
    Ok(())
}

fn close_device(
    device: &mut Option<MicroDevice>,
    state: &mut State,
    gestures: &mut GestureDispatcher,
    blank: bool,
) {
    gestures.clear();
    state.last_lighting.clear();
    state.last_focused_app.clear();
    state.last_joystick_sector = None;
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
    state.managed_aggregate_zones.clear();
    let _ = device.close();
}

fn select_layer(
    device: Option<&MicroDevice>,
    state: &mut State,
    layer: Option<usize>,
) -> Result<()> {
    let Some(layer) = layer else { return Ok(()) };
    state.active_layer = Some(layer);
    let Some(device) = device else { return Ok(()) };
    let app = layer_identity(layer);
    let signature = serde_json::to_string(&app)?;
    if signature != state.last_focused_app {
        device.send("host.focused_app", Some(serde_json::to_value(app)?))?;
        state.last_focused_app = signature;
        log(format!("layer {layer} selected"));
    }
    Ok(())
}

fn select_session(
    device: Option<&MicroDevice>,
    state: &mut State,
    gestures: &mut GestureDispatcher,
    next: Option<String>,
) -> Result<()> {
    if state.selected_session == next {
        return Ok(());
    }
    state.routing_ready = false;
    state.routing_generation.fetch_add(1, Ordering::AcqRel);
    state.selected_session = next;
    state.agents.clear();
    state.slots.fill(None);
    state.last_lighting.clear();
    if let Some(device) = device {
        send_lighting(device, state)?;
    }
    log(state
        .selected_session
        .as_ref()
        .map(|session| format!("Herdr session selected: {session}"))
        .unwrap_or_else(|| "Herdr session unselected".into()));
    gestures.clear();
    Ok(())
}

fn routing_failure(state: &mut State, message: impl Into<String>) {
    let message = message.into();
    if state.last_routing_error != message {
        state.last_routing_error = message.clone();
        log(format!("Herdr session routing failed: {message}"));
    }
}

fn refresh_sessions(
    state: &mut State,
    base: &Environment,
    device: Option<&MicroDevice>,
    gestures: &mut GestureDispatcher,
    mappings: &Arc<Mutex<Vec<SessionTerminalMapping>>>,
) -> Result<bool> {
    let discovered = match discover_sessions(base) {
        Ok(sessions) => sessions,
        Err(error) => {
            routing_failure(state, error.to_string());
            state.revoke_routing();
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
    state.sessions = discovered;
    if state
        .selected_session
        .as_ref()
        .is_some_and(|session| !state.sessions.contains(session))
    {
        select_session(device, state, gestures, None)?;
    }
    if changed {
        state.mappings.clear();
        *mappings.lock().unwrap_or_else(|error| error.into_inner()) = Vec::new();
        let _ = refresh_mappings(state, base, mappings)?;
    }
    Ok(state
        .no_sessions_at
        .is_some_and(|at| now.duration_since(at) >= NO_SESSIONS_SHUTDOWN))
}

fn refresh_mappings(
    state: &mut State,
    base: &Environment,
    mappings: &Arc<Mutex<Vec<SessionTerminalMapping>>>,
) -> Result<bool> {
    state.next_mapping_probe = Some(Instant::now() + MAPPING_REPROBE);
    if state.sessions.is_empty() {
        state.mappings.clear();
        *mappings.lock().unwrap_or_else(|error| error.into_inner()) = Vec::new();
        return Ok(true);
    }
    state.revoke_routing();
    match probe_session_terminals(&state.sessions, base) {
        Ok(found) => {
            state.mappings = found;
            *mappings.lock().unwrap_or_else(|error| error.into_inner()) = state.mappings.clone();
            state.last_routing_error.clear();
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
        Err(error) => {
            state.mappings.clear();
            *mappings.lock().unwrap_or_else(|error| error.into_inner()) = Vec::new();
            state.next_mapping_probe = Some(Instant::now() + REFRESH_INTERVAL);
            routing_failure(state, error.to_string());
            return Ok(false);
        }
    }
    Ok(true)
}

fn refresh_routing(
    state: &mut State,
    base: &Environment,
    device: Option<&MicroDevice>,
    gestures: &mut GestureDispatcher,
    mappings: &Arc<Mutex<Vec<SessionTerminalMapping>>>,
) -> Result<bool> {
    let Some(frontmost_process) = state
        .frontmost
        .as_ref()
        .map(|frontmost| frontmost.process.clone())
    else {
        state.revoke_routing();
        return Ok(false);
    };
    if frontmost_process != crate::actions::GHOSTTY_PROCESS {
        state.ghostty = None;
        state.revoke_routing();
        select_layer(
            device,
            state,
            automatic_layer(Some(&frontmost_process), None),
        )?;
        return Ok(false);
    }
    match inspect_ghostty() {
        Ok(mut ghostty) => {
            if state
                .next_mapping_probe
                .is_some_and(|due| Instant::now() >= due)
                && !refresh_mappings(state, base, mappings)?
            {
                state.revoke_routing();
                return Ok(false);
            }
            let mut session = focused_session(&state.mappings, &ghostty);
            if session.is_none() && state.next_mapping_probe.is_none() {
                if !refresh_mappings(state, base, mappings)? {
                    state.revoke_routing();
                    return Ok(false);
                }
                ghostty = match inspect_ghostty() {
                    Ok(ghostty) => ghostty,
                    Err(error) => {
                        routing_failure(state, error.to_string());
                        state.revoke_routing();
                        return Ok(false);
                    }
                };
                session = focused_session(&state.mappings, &ghostty);
            }
            state.ghostty = Some(ghostty);
            let Some(session) = session.filter(|session| state.sessions.contains(session)) else {
                state.revoke_routing();
                return Ok(false);
            };
            select_session(device, state, gestures, Some(session.clone()))?;
            select_layer(
                device,
                state,
                automatic_layer(Some(&frontmost_process), Some(&session)),
            )?;
            state.last_routing_error.clear();
            return Ok(true);
        }
        Err(error) => {
            routing_failure(state, error.to_string());
            state.revoke_routing();
        }
    }
    Ok(false)
}

fn refresh_agents(
    state: &mut State,
    base: &Environment,
    device: Option<&MicroDevice>,
) -> Result<()> {
    let Some(session) = state.selected_session.clone() else {
        return Ok(());
    };
    match list_agents(&session, base) {
        Ok(agents) => {
            if state.selected_session.as_deref() != Some(&session) {
                return Ok(());
            }
            if agent_identity(state.agents.iter().find(|agent| agent.focused))
                != agent_identity(agents.iter().find(|agent| agent.focused))
            {
                state.revoke_routing();
            }
            let previous = state
                .session_slots
                .get(&session)
                .cloned()
                .unwrap_or_default();
            let slots = assign_slots(&previous, &agents);
            state.session_slots.insert(session, slots.clone());
            state.agents = agents;
            state.slots = slots;
            state.routing_ready = true;
            state.last_herdr_error.clear();
            if let Some(device) = device {
                send_lighting(device, state)?;
            }
        }
        Err(error) => {
            if state.selected_session.as_deref() != Some(&session) {
                return Ok(());
            }
            let had_state = state.routing_ready
                || !state.agents.is_empty()
                || state.slots.iter().any(Option::is_some);
            state.revoke_routing();
            state.agents.clear();
            state.slots.fill(None);
            if had_state {
                state.last_lighting.clear();
                if let Some(device) = device {
                    send_lighting(device, state)?;
                }
            }
            if state.last_herdr_error != error.to_string() {
                state.last_herdr_error = error.to_string();
                log(format!("Herdr session {session} unavailable: {error}"));
            }
        }
    }
    Ok(())
}

fn refresh_frontmost(state: &mut State) {
    match macos::frontmost() {
        Ok(frontmost) => {
            state.frontmost = Some(frontmost);
            state.last_frontmost_error.clear();
        }
        Err(error) if state.last_frontmost_error != error.to_string() => {
            state.frontmost = None;
            state.revoke_routing();
            state.last_frontmost_error = error.to_string();
            log(format!("frontmost window unavailable: {error}"));
        }
        Err(_) => {
            state.frontmost = None;
            state.revoke_routing();
        }
    }
}

fn refresh_owner(
    state: &mut State,
    device: &mut Option<MicroDevice>,
    gestures: &mut GestureDispatcher,
) -> Result<()> {
    let frontmost = state
        .frontmost
        .as_ref()
        .map(|frontmost| frontmost.process.clone());
    let owner = match device_owner(&[], frontmost.as_deref()) {
        Some(owner) => Some(owner),
        None => {
            let output = run_command("/bin/ps", &["-axo".into(), "command=".into()], None)?;
            let processes: Vec<_> = output.lines().map(str::trim).map(str::to_owned).collect();
            device_owner(&processes, frontmost.as_deref())
        }
    }
    .map(str::to_owned);
    if owner != state.owner {
        let handoff_error = if owner.as_deref() == Some("ChatGPT") {
            select_layer(
                device.as_ref(),
                state,
                automatic_layer(frontmost.as_deref(), None),
            )
            .err()
        } else {
            None
        };
        state.owner = owner;
        if let Some(owner) = &state.owner {
            log(format!("yielding to {owner}"));
            state.revoke_routing();
            close_device(device, state, gestures, false);
        } else {
            log("device owner cleared");
        }
        if let Some(error) = handoff_error {
            return Err(error);
        }
    }
    Ok(())
}

fn open_device(
    device: &mut Option<MicroDevice>,
    event_tx: &Sender<DeviceEvent>,
    state: &mut State,
) -> Result<()> {
    if device.is_some() || state.owner.is_some() {
        return Ok(());
    }
    match MicroDevice::open(event_tx.clone()) {
        Ok(opened) => {
            *device = Some(opened);
            state.device_state = "connected".into();
            state.last_open_error.clear();
            state.last_lighting.clear();
            log("device connected");
            select_layer(device.as_ref(), state, state.active_layer)?;
            if let Some(device) = device.as_ref() {
                send_lighting(device, state)?;
            }
        }
        Err(error) => {
            let message = error.to_string();
            state.device_state = if message.contains("Input Monitoring") {
                "permission-denied"
            } else if message.contains("not found") {
                "absent"
            } else {
                "busy"
            }
            .into();
            if state.last_open_error != message {
                state.last_open_error = message.clone();
                log(format!("device open failed: {message}"));
            }
        }
    }
    Ok(())
}

fn publish(status: &Arc<Mutex<Value>>, state: &State) {
    *status.lock().unwrap_or_else(|error| error.into_inner()) = state.status();
}

/// Run the bridge in the foreground.  `main`/the start action owns process
/// detachment; this function deliberately owns only the live daemon.
pub fn run_daemon() -> Result<()> {
    let base = current_environment();
    let controls_path = config_path("controls.json");
    provision_controls(&controls_path).map_err(|error| anyhow!(error))?;
    provision_effort(&config_path("effort.json")).map_err(|error| anyhow!(error))?;
    provision_lighting(&config_path("lighting.json")).map_err(|error| anyhow!(error))?;
    let mut controls = load_controls(&controls_path).map_err(|error| anyhow!(error))?;
    let active_hid_keys = controls.hid_keys.clone();
    let (device_tx, device_rx) = mpsc::channel();
    let (work_tx, work_rx) = mpsc::sync_channel(WORK_QUEUE_CAPACITY);
    let mappings = Arc::new(Mutex::new(Vec::new()));
    let stopping = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, Arc::clone(&stopping))?;
    signal_hook::flag::register(SIGTERM, Arc::clone(&stopping))?;
    let mut state = State::new();
    let worker = thread::spawn({
        let mappings = Arc::clone(&mappings);
        let base = base.clone();
        let routing_generation = Arc::clone(&state.routing_generation);
        let stopping = Arc::clone(&stopping);
        move || action_worker(work_rx, base, mappings, routing_generation, stopping)
    });
    let status = Arc::new(Mutex::new(state.status()));
    let server = listen_for_control(
        {
            let status = Arc::clone(&status);
            move || {
                status
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone()
            }
        },
        {
            let stopping = Arc::clone(&stopping);
            move || stopping.store(true, Ordering::Release)
        },
    )?;
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let control_thread = thread::spawn(move || {
        let _ = server.run_with_shutdown(&shutdown_rx);
    });
    let mut device = None;
    let mut gestures = GestureDispatcher::new();
    let mut refresh_due = Instant::now();
    log("bridge started");
    while !stopping.load(Ordering::Acquire) {
        let now = Instant::now();
        if now >= refresh_due {
            refresh_due = now + REFRESH_INTERVAL;
            let routing_generation = state.routing_generation();
            match load_controls(&controls_path) {
                Ok(next) => {
                    let (next, hid_keys_pending) = runtime_controls(&active_hid_keys, next);
                    if controls != next {
                        gestures.clear();
                    }
                    controls = next;
                    if hid_keys_pending {
                        let pending = "HID key changes require micro-setup and a bridge restart";
                        if state.last_controls_error != pending {
                            log(format!("control configuration pending: {pending}"));
                        }
                        state.last_controls_error = pending.into();
                    } else {
                        state.last_controls_error.clear();
                    }
                }
                Err(error) if state.last_controls_error != error => {
                    state.last_controls_error = error.clone();
                    log(format!("control configuration failed: {error}"));
                }
                Err(_) => {}
            }
            refresh_frontmost(&mut state);
            let owner_checked = match refresh_owner(&mut state, &mut device, &mut gestures) {
                Ok(()) => true,
                Err(error) => {
                    state.revoke_routing();
                    log(format!("owner refresh failed: {error:#}"));
                    false
                }
            };
            let mut shutdown = false;
            if owner_checked && state.owner.is_none() {
                let sessions_refreshed = match refresh_sessions(
                    &mut state,
                    &base,
                    device.as_ref(),
                    &mut gestures,
                    &mappings,
                ) {
                    Ok(next_shutdown) => {
                        shutdown = next_shutdown;
                        true
                    }
                    Err(error) => {
                        log(format!("refresh failed: {error:#}"));
                        false
                    }
                };
                if sessions_refreshed {
                    let routing_ready = match refresh_routing(
                        &mut state,
                        &base,
                        device.as_ref(),
                        &mut gestures,
                        &mappings,
                    ) {
                        Ok(ready) => ready,
                        Err(error) => {
                            state.revoke_routing();
                            log(format!("refresh failed: {error:#}"));
                            false
                        }
                    };
                    if let Err(error) = open_device(&mut device, &device_tx, &mut state) {
                        log(format!("refresh failed: {error:#}"));
                    }
                    if routing_ready {
                        if let Err(error) = refresh_agents(&mut state, &base, device.as_ref()) {
                            log(format!("refresh failed: {error:#}"));
                        }
                    }
                }
            }
            if routing_generation != state.routing_generation() {
                gestures.clear();
                while let Ok(event) = device_rx.try_recv() {
                    if matches!(event, DeviceEvent::Disconnected) {
                        log("device disconnected");
                        close_device(&mut device, &mut state, &mut gestures, false);
                        break;
                    }
                }
            }
            publish(&status, &state);
            if shutdown {
                log("No Herdr sessions for 60 seconds; releasing device");
                stopping.store(true, Ordering::Release);
            }
        }
        match device_rx.try_recv() {
            Ok(event) => {
                if handle_device_event(event, &mut state, &controls, &mut gestures, &work_tx) {
                    close_device(&mut device, &mut state, &mut gestures, false);
                }
                continue;
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        handle_fired(
            &work_tx,
            state.agents.iter().find(|agent| agent.focused).cloned(),
            state.routing_generation(),
            gestures.drain_due(Instant::now()),
        );
        let deadline = gestures
            .next_deadline()
            .map(|deadline| deadline.min(refresh_due))
            .unwrap_or(refresh_due);
        match device_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(event) => {
                if handle_device_event(event, &mut state, &controls, &mut gestures, &work_tx) {
                    close_device(&mut device, &mut state, &mut gestures, false);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    log("stopping");
    state.revoke_routing();
    stopping.store(true, Ordering::Release);
    close_device(&mut device, &mut state, &mut gestures, true);
    publish(&status, &state);
    drop(work_tx);
    let _ = worker.join();
    let _ = shutdown_tx.send(());
    let _ = control_thread.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_iso_8601() {
        assert_eq!(format_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            format_timestamp(UNIX_EPOCH + Duration::from_millis(946_782_245_678)),
            "2000-01-02T03:04:05.678Z"
        );
    }

    #[test]
    fn status_matches_the_control_contract() {
        let mut state = State::new();
        state.selected_session = Some("work".into());
        state.routing_ready = true;
        state.sessions = vec!["work".into()];
        state.mappings = vec![SessionTerminalMapping {
            session_name: "work".into(),
            terminal_id: "t1".into(),
        }];
        let status = state.status();
        assert_eq!(status["routing"], "ready");
        assert_eq!(
            status["sessionMappings"],
            json!([{"session":"work","terminal":"t1"}])
        );
        assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(status["protocol"], DAEMON_PROTOCOL_VERSION);
        assert_eq!(status.as_object().unwrap().len(), 13);
    }

    #[test]
    fn key_events_capture_the_ready_session_without_hardware() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut state = State::new();
        state.selected_session = Some("work".into());
        state.routing_ready = true;
        state.agents.push(Agent {
            terminal_id: "terminal".into(),
            pane_id: "pane".into(),
            agent: "codex".into(),
            agent_status: AgentStatus::Idle,
            state_change_seq: 1,
            focused: true,
            cwd: "/tmp".into(),
        });
        let controls = crate::config::default_controls();
        let mut gestures = GestureDispatcher::new();
        assert!(!handle_device_event(
            DeviceEvent::Key {
                key: "ACT12".into(),
                action: 1
            },
            &mut state,
            &controls,
            &mut gestures,
            &sender
        ));
        match receiver.recv_timeout(Duration::from_millis(100)).unwrap() {
            Work::Binding {
                session, target, ..
            } => {
                assert_eq!(session, "work");
                assert_eq!(
                    agent_identity(target.as_ref()),
                    Some(("terminal", "pane", "codex"))
                );
            }
            _ => panic!("expected configured binding"),
        }
    }

    #[test]
    fn revoking_routing_invalidates_queued_work() {
        let mut state = State::new();
        state.routing_ready = true;
        let queued = state.routing_generation();
        state.revoke_routing();
        assert_ne!(queued, state.routing_generation());
    }

    #[test]
    fn hid_key_reload_waits_for_a_restart() {
        let initial = crate::config::default_controls();
        let active = initial.hid_keys.clone();
        let mut changed = initial.clone();
        changed.hid_keys.insert(5, Some("F17".into()));
        let (runtime, pending) = runtime_controls(&active, changed);
        assert!(pending);
        assert_eq!(runtime.hid_keys, initial.hid_keys);
    }
}
