//! The long-lived Codex Micro bridge.  Keep the policy here; HID framing,
//! Herdr parsing, gestures, and macOS operations live in their small modules.

use anyhow::{Result, anyhow, bail};
use herdr_client::{AgentInfo, Client, SessionSnapshot, open_rotating_log};
use serde_json::{Value, json};
use signal_hook::consts::{SIGINT, SIGTERM};
use std::io::Write;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    actions::{
        GHOSTTY_PROCESS, automatic_layer, focus_agent, focus_pane, layer_identity, open_diff,
        plan_effort_change, prompt as send_prompt, scroll_plan, submit,
    },
    config::{
        Action, Binding, Config, Controls, Direction, EffortConfig, EffortDirection, Modifier,
        VerticalDirection, config_path, enabled_buttons, key_action_code, key_binding, load,
        provision,
    },
    control::listen_for_control,
    device::DeviceEvent,
    gestures::{Fired, GestureDispatcher},
    ghostty::{
        SessionTerminalMapping, focused_terminal_id, probe_session_terminals, scroll_terminal,
    },
    herdr::{
        Session, SessionUpdate, SessionWorker, current_snapshot, discover_sessions,
        spawn_session_worker,
    },
    hid::{HidClient, connection_error_state},
    macos,
    protocol::{
        INPUT_BUNDLE_ID, SLOT_COUNT, aggregate_lighting, assign_slots, device_owner,
        joystick_event, slot_lighting,
    },
};

const CONFIG_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const SESSION_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const SESSION_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const MAPPING_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const ROUTING_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
const DEVICE_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const NO_SESSIONS_SHUTDOWN: Duration = Duration::from_secs(60);
const WORK_QUEUE_CAPACITY: usize = 16;
pub const DAEMON_PROTOCOL_VERSION: u32 = 2;

static LOG_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub fn log(message: impl AsRef<str>) {
    let _guard = LOG_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let line = format!(
        "{} {}",
        format_timestamp(SystemTime::now()),
        message.as_ref()
    );
    if let Ok(path) = crate::control::log_file()
        && let Ok(mut file) = open_rotating_log(&path, 10 << 20, 3)
        && writeln!(file, "{line}").is_ok()
    {
        return;
    }
    eprintln!("{line}");
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
    config: Config,
    sessions: Vec<Session>,
    session_agents: HashMap<String, Vec<AgentInfo>>,
    session_epochs: HashMap<String, String>,
    mappings: Vec<SessionTerminalMapping>,
    selected_session: Option<String>,
    routing_verified: bool,
    routing_ready: bool,
    routing_generation: Arc<AtomicU64>,
    session_slots: HashMap<String, Vec<Option<String>>>,
    agents: Vec<AgentInfo>,
    slots: Vec<Option<String>>,
    device_state: String,
    owner: Option<String>,
    frontmost: Option<macos::Frontmost>,
    focused_terminal: Option<String>,
    active_layer: Option<usize>,
    last_device_error: String,
    last_herdr_error: String,
    last_frontmost_error: String,
    last_controls_error: String,
    last_routing_error: String,
    last_lighting_error: String,
    last_lighting: String,
    last_focused_app: String,
    managed_aggregate_zones: HashSet<String>,
    no_sessions_at: Option<Instant>,
    next_mapping_probe: Option<Instant>,
    next_device_open: Option<Instant>,
    device_restore_pending: bool,
}

impl State {
    fn new(config: Config) -> Self {
        Self {
            config,
            slots: vec![None; SLOT_COUNT],
            device_state: "starting".into(),
            ..Self::default()
        }
    }

    fn status(&self) -> Value {
        json!({
            "device": if self.owner.is_some() { "yielded" } else { &self.device_state },
            "deviceError": (!self.last_device_error.is_empty()).then_some(&self.last_device_error),
            "owner": self.owner,
            "session": self.selected_session,
            "routing": if self.routing_ready { "ready" } else if self.selected_session.is_some() { "unavailable" } else { "none" },
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
                "title": frontmost.title,
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

    fn revoke_routing(&mut self) {
        self.routing_verified = false;
        self.invalidate_routing();
    }

    fn invalidate_routing(&mut self) {
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
struct InputContext {
    controls: Controls,
    effort: EffortConfig,
    session: Option<Session>,
    session_epoch: Option<String>,
    terminal: Option<String>,
    target: Option<AgentInfo>,
    slots: Vec<Option<AgentInfo>>,
    routing_ready: bool,
    generation: u64,
}

impl InputContext {
    fn new(config: &Config) -> Self {
        Self {
            controls: config.controls.clone(),
            effort: config.effort.clone(),
            session: None,
            session_epoch: None,
            terminal: None,
            target: None,
            slots: vec![None; SLOT_COUNT],
            routing_ready: false,
            generation: 0,
        }
    }

    fn selected(&self, routing_generation: &AtomicU64) -> Option<(Session, String, String)> {
        (self.routing_ready && self.generation == routing_generation.load(Ordering::Acquire))
            .then(|| {
                self.session
                    .clone()
                    .zip(self.terminal.clone())
                    .zip(self.session_epoch.clone())
                    .map(|((session, terminal), epoch)| (session, terminal, epoch))
            })
            .flatten()
    }
}

#[derive(Default)]
struct InputState {
    gestures: GestureDispatcher,
    last_joystick_sector: Option<u8>,
}

#[derive(Clone)]
enum Work {
    Binding {
        binding: Box<Binding>,
        source: String,
        session: Session,
        session_epoch: String,
        terminal: String,
        effort: EffortConfig,
        target: Option<(String, String, Option<String>)>,
        generation: u64,
    },
    FocusSlot {
        pane_id: String,
        source: String,
        session: Session,
        session_epoch: String,
        terminal: String,
        generation: u64,
    },
}

impl Work {
    fn generation(&self) -> u64 {
        match self {
            Self::Binding { generation, .. } | Self::FocusSlot { generation, .. } => *generation,
        }
    }

    fn same_effort(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Binding {
                    binding: first,
                    session: first_session,
                    session_epoch: first_epoch,
                    terminal: first_terminal,
                    effort: first_effort,
                    target: first_target,
                    generation: first_generation,
                    ..
                },
                Self::Binding {
                    binding: second,
                    session: second_session,
                    session_epoch: second_epoch,
                    terminal: second_terminal,
                    effort: second_effort,
                    target: second_target,
                    generation: second_generation,
                    ..
                },
            ) => match (first.as_ref(), second.as_ref()) {
                (
                    Binding::Action(Action::Effort {
                        direction: first_direction,
                    }),
                    Binding::Action(Action::Effort {
                        direction: second_direction,
                    }),
                ) => {
                    first_direction == second_direction
                        && first_session == second_session
                        && first_epoch == second_epoch
                        && first_terminal == second_terminal
                        && first_effort == second_effort
                        && first_target == second_target
                        && first_generation == second_generation
                }
                _ => false,
            },
            _ => false,
        }
    }
}

fn require_agent(agent: Option<&AgentInfo>) -> Result<&AgentInfo> {
    agent.ok_or_else(|| anyhow!("no focused Herdr agent"))
}

fn agent_identity(agent: Option<&AgentInfo>) -> Option<(String, String, Option<String>)> {
    agent.map(|agent| {
        (
            agent.terminal_id.clone(),
            agent.pane_id.clone(),
            agent.agent.clone(),
        )
    })
}

fn ready_agent(agent: Option<&AgentInfo>) -> Result<&AgentInfo> {
    let agent = require_agent(agent)?;
    if matches!(agent.agent_status.as_str(), "idle" | "done") {
        Ok(agent)
    } else {
        bail!("focused agent is {}", agent.agent_status)
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
        Action::Key { .. } => "key",
    }
}

struct DispatchLease<'a> {
    session: &'a Session,
    session_epoch: &'a str,
    terminal: &'a str,
    generation: u64,
    routing_generation: &'a AtomicU64,
}

impl DispatchLease<'_> {
    fn client(&self) -> Client {
        self.session.client().with_session_epoch(self.session_epoch)
    }

    fn ensure(&self) -> Result<String> {
        if self.generation != self.routing_generation.load(Ordering::Acquire) {
            bail!("stale Herdr routing")
        }
        if !macos::frontmost_bundle_is(GHOSTTY_PROCESS) {
            bail!("Herdr session is no longer frontmost")
        }
        let terminal = focused_terminal_id()?;
        if self.generation != self.routing_generation.load(Ordering::Acquire)
            || !macos::frontmost_bundle_is(GHOSTTY_PROCESS)
        {
            bail!("stale Herdr routing")
        }
        if terminal != self.terminal {
            bail!("Herdr session is no longer frontmost")
        }
        Ok(terminal)
    }
}

fn execute_scroll(
    direction: VerticalDirection,
    percent: f64,
    expected_pane: &str,
    snapshot: &SessionSnapshot,
    lease: &DispatchLease<'_>,
) -> Result<bool> {
    if snapshot.focused_pane_id.as_deref() != Some(expected_pane) {
        log(format!(
            "scroll ignored: focused pane changed in {}",
            lease.session.name
        ));
        return Ok(false);
    }
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == expected_pane)
        .ok_or_else(|| anyhow!("focused pane disappeared"))?;
    let layout = snapshot
        .layouts
        .iter()
        .find(|layout| layout.workspace_id == pane.workspace_id && layout.tab_id == pane.tab_id)
        .ok_or_else(|| anyhow!("focused pane layout unavailable"))?;
    let plan = scroll_plan(
        pane,
        layout,
        match direction {
            VerticalDirection::Up => "up",
            VerticalDirection::Down => "down",
        },
        percent,
    )?;
    let cell = lease
        .client()
        .call_value("pane.graphics.info", &json!({ "pane_id": expected_pane }))?;
    let cell_width = cell
        .get("cell_width_px")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("Herdr host cell width is unavailable"))?;
    let cell_height = cell
        .get("cell_height_px")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("Herdr host cell height is unavailable"))?;
    let scale = macos::frontmost_window_scale()?;
    let terminal = lease.ensure()?;
    scroll_terminal(
        &terminal,
        plan.column * cell_width as f64 / scale,
        plan.row * cell_height as f64 / scale,
        i32::try_from(plan.notches).map_err(|_| anyhow!("scroll distance is too large"))?,
    )?;
    log(format!(
        "scrolled {} {percent}% in {}/{}",
        match direction {
            VerticalDirection::Up => "up",
            VerticalDirection::Down => "down",
        },
        lease.session.name,
        plan.pane_id
    ));
    Ok(true)
}

fn execute_action(
    action: &Action,
    current: Option<&AgentInfo>,
    snapshot: &SessionSnapshot,
    effort: &EffortConfig,
    repeat: usize,
    lease: &DispatchLease<'_>,
) -> Result<bool> {
    let client = lease.client();
    match action {
        Action::Prompt { prompt, submit } => {
            let current = ready_agent(current)?;
            lease.ensure()?;
            send_prompt(&client, prompt, submit.unwrap_or(true), current)?;
        }
        Action::Diff => {
            let current = require_agent(current)?;
            lease.ensure()?;
            open_diff(&client, current)?;
        }
        Action::Fast => {
            let current = ready_agent(current)?;
            match current.agent.as_deref().unwrap_or_default() {
                "codex" => {
                    lease.ensure()?;
                    send_prompt(&client, "/fast", true, current)?;
                }
                "pi" => {
                    lease.ensure()?;
                    send_prompt(&client, "/fast", false, current)?;
                    lease.ensure()?;
                    submit(&client, current)?;
                }
                other => bail!(
                    "unsupported focused agent: {}",
                    if other.is_empty() { "none" } else { other }
                ),
            }
        }
        Action::Submit => {
            let current = require_agent(current)?;
            lease.ensure()?;
            submit(&client, current)?;
        }
        Action::Effort { direction } => {
            let current = require_agent(current)?;
            let direction = match direction {
                EffortDirection::Raise => "raise",
                EffortDirection::Lower => "lower",
            };
            let plan = plan_effort_change(
                current.agent.as_deref().unwrap_or_default(),
                direction,
                &current.pane_id,
                effort,
                repeat,
            )?;
            for step in plan {
                lease.ensure()?;
                client.call_value(step.method, &step.params)?;
                if let Some(ms) = step.wait_after_ms {
                    thread::sleep(Duration::from_millis(ms));
                }
            }
        }
        Action::FocusPane { direction } => {
            let direction = match direction {
                Direction::Up => "up",
                Direction::Down => "down",
                Direction::Left => "left",
                Direction::Right => "right",
            };
            let pane = require_agent(current)?.pane_id.clone();
            lease.ensure()?;
            focus_pane(&client, &pane, direction)?;
            log(format!(
                "joystick focus {direction}: {}/{pane}",
                lease.session.name
            ));
        }
        Action::Scroll { direction, percent } => {
            return execute_scroll(
                *direction,
                *percent,
                &require_agent(current)?.pane_id,
                snapshot,
                lease,
            );
        }
        // Key actions cannot reach the worker: top-level keys execute inline in
        // queue_binding and byAgent keys are rejected at parse.
        Action::Key { .. } => bail!("key action routed to the session worker"),
    }
    Ok(true)
}

fn action_worker(
    receiver: Receiver<Work>,
    routing_generation: Arc<AtomicU64>,
    stopping: Arc<AtomicBool>,
) {
    let mut deferred = None;
    while !stopping.load(Ordering::Acquire) {
        let work = match deferred
            .take()
            .map(Ok)
            .unwrap_or_else(|| receiver.recv_timeout(Duration::from_millis(50)))
        {
            Ok(work) => work,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut repeat = 1;
        while let Ok(next) = receiver.try_recv() {
            if work.same_effort(&next) {
                repeat += 1;
            } else {
                deferred = Some(next);
                break;
            }
        }
        let generation = work.generation();
        if generation != routing_generation.load(Ordering::Acquire) {
            log("control ignored: stale Herdr routing");
            continue;
        }
        let (session_info, session_epoch, terminal) = match &work {
            Work::Binding {
                session,
                session_epoch,
                terminal,
                ..
            }
            | Work::FocusSlot {
                session,
                session_epoch,
                terminal,
                ..
            } => (session.clone(), session_epoch.clone(), terminal.clone()),
        };
        let snapshot = match current_snapshot(&session_info.client()) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                log(format!(
                    "control failed: refresh {}: {error:#}",
                    session_info.name
                ));
                continue;
            }
        };
        if snapshot.session_epoch != session_epoch {
            log("control ignored: Herdr session was replaced");
            continue;
        }
        if generation != routing_generation.load(Ordering::Acquire) {
            log("control ignored: stale Herdr routing");
            continue;
        }
        let lease = DispatchLease {
            session: &session_info,
            session_epoch: &session_epoch,
            terminal: &terminal,
            generation,
            routing_generation: &routing_generation,
        };
        let result = match work {
            Work::FocusSlot {
                pane_id,
                source,
                session,
                ..
            } => {
                let result = if snapshot.agents.iter().any(|agent| agent.pane_id == pane_id) {
                    lease
                        .ensure()
                        .and_then(|_| focus_agent(&lease.client(), &pane_id))
                } else {
                    Err(anyhow!("Agent slot pane disappeared"))
                };
                if result.is_ok() {
                    log(format!("{source}: focused {}/{pane_id}", session.name));
                }
                result
            }
            Work::Binding {
                binding,
                source,
                session,
                effort,
                target,
                ..
            } => (|| {
                let current = snapshot.agents.iter().find(|agent| agent.focused);
                if agent_identity(current) != target {
                    log(format!("{source} ignored: focused pane changed"));
                    return Ok(());
                }
                let Some(action) = binding.resolve(
                    current
                        .and_then(|agent| agent.agent.as_deref())
                        .unwrap_or(""),
                ) else {
                    return Ok(());
                };
                let executed =
                    execute_action(&action, current, &snapshot, &effort, repeat, &lease)?;
                if executed {
                    log(format!(
                        "{source}: {}{} in {}{}",
                        action_name(&action),
                        if repeat > 1 {
                            format!(" x{repeat}")
                        } else {
                            String::new()
                        },
                        session.name,
                        current
                            .map(|agent| format!(
                                " for {} in {}",
                                agent.agent.as_deref().unwrap_or("unknown"),
                                agent.pane_id
                            ))
                            .unwrap_or_default()
                    ));
                }
                Ok(())
            })(),
        };
        if let Err(error) = result {
            log(format!("control failed: {error:#}"));
        }
    }
}

fn execute_key(key: Option<&str>, keycode: Option<u16>, modifiers: &[Modifier]) -> Result<()> {
    let code = key_action_code(key, keycode).map_err(|error| anyhow!(error))?;
    macos::post_key(code, modifiers)
}

fn queue_binding(
    sender: &SyncSender<Work>,
    binding: Binding,
    source: String,
    route: Option<(Session, String, String)>,
    effort: &EffortConfig,
    target: Option<AgentInfo>,
    generation: u64,
) {
    // Key taps are system-wide by nature: fire from any frontmost app while
    // the bridge owns the device, and never wait on the action queue.
    if let Binding::Action(Action::Key {
        key,
        keycode,
        modifiers,
    }) = &binding
    {
        match execute_key(key.as_deref(), *keycode, modifiers) {
            Ok(()) => log(format!("{source}: key tap")),
            Err(error) => log(format!("{source} key tap failed: {error:#}")),
        }
        return;
    }
    match route {
        Some((session, terminal, session_epoch)) => {
            let work = Work::Binding {
                binding: Box::new(binding),
                source,
                session,
                session_epoch,
                terminal,
                effort: effort.clone(),
                target: agent_identity(target.as_ref()),
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
    context: &InputContext,
    routing_generation: &AtomicU64,
    fired: Vec<Fired>,
) {
    for fired in fired {
        let route = context
            .selected(routing_generation)
            .filter(|(session, _, _)| {
                fired
                    .context
                    .as_deref()
                    .is_some_and(|name| name == session.name)
            });
        queue_binding(
            sender,
            Binding::Action(fired.action),
            fired.source,
            route,
            &context.effort,
            context.target.clone(),
            context.generation,
        );
    }
}

fn agent_slot(key: &str) -> Option<usize> {
    key.strip_prefix("AG0")
        .and_then(|index| index.parse::<usize>().ok())
        .filter(|index| *index < SLOT_COUNT)
}

fn device_failure(state: &mut State, context: &str, error: String) {
    state.device_state = "helper-error".into();
    if state.last_device_error != error {
        log(format!("{context}: {error}"));
        state.last_device_error = error;
    }
}

fn device_disconnected(state: &mut State, error: String) {
    device_failure(state, "device disconnected", error);
    state.device_restore_pending = true;
    state.next_device_open = Some(Instant::now());
}

fn handle_device_event(
    event: DeviceEvent,
    state: &mut InputState,
    context: &InputContext,
    routing_generation: &AtomicU64,
    worker: &SyncSender<Work>,
    notices: &Sender<String>,
) {
    let controls = &context.controls;
    match event {
        DeviceEvent::Disconnected { error } => {
            state.gestures.clear();
            state.last_joystick_sector = None;
            let _ = notices.send(error);
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
            let (action, direction) = match next.direction {
                Some(Direction::Up) => (&controls.joystick.up, "up"),
                Some(Direction::Down) => (&controls.joystick.down, "down"),
                Some(Direction::Left) => (&controls.joystick.left, "left"),
                Some(Direction::Right) => (&controls.joystick.right, "right"),
                None => return,
            };
            if let Some(action) = action.clone() {
                queue_binding(
                    worker,
                    Binding::Action(action),
                    format!("joystick {direction}"),
                    context.selected(routing_generation),
                    &context.effort,
                    context.target.clone(),
                    context.generation,
                );
            }
        }
        DeviceEvent::Key { key, action } => {
            if let Some(index) = agent_slot(&key) {
                if action == 1 {
                    let session = context.selected(routing_generation);
                    let agent = context.slots[index].as_ref();
                    match (session, agent) {
                        (Some((session, terminal, session_epoch)), Some(agent)) => {
                            let work = Work::FocusSlot {
                                pane_id: agent.pane_id.clone(),
                                source: key,
                                session,
                                session_epoch,
                                terminal,
                                generation: context.generation,
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
                return;
            }
            let binding = key_binding(controls, &key, action);
            let captured = context.selected(routing_generation);
            let gesture_context = captured
                .as_ref()
                .map(|(session, _, _)| session.name.clone());
            if matches!(key.as_str(), "ENC_CC" | "ENC_CW") {
                if let Some(binding) = binding {
                    queue_binding(
                        worker,
                        binding,
                        key,
                        captured,
                        &context.effort,
                        context.target.clone(),
                        context.generation,
                    );
                }
            } else if matches!(action, 0 | 1) {
                match binding.as_ref() {
                    Some(Binding::Gesture(_)) => handle_fired(
                        worker,
                        context,
                        routing_generation,
                        state.gestures.handle(
                            key,
                            binding.as_ref(),
                            action == 1,
                            gesture_context,
                            Instant::now(),
                        ),
                    ),
                    Some(_) if action == 1 => queue_binding(
                        worker,
                        binding.unwrap(),
                        key,
                        captured,
                        &context.effort,
                        context.target.clone(),
                        context.generation,
                    ),
                    _ => {}
                }
            }
        }
    }
}

fn refresh_input_context(
    shared: &Mutex<Arc<InputContext>>,
    context: &mut Arc<InputContext>,
    state: &mut InputState,
    routing_generation: &AtomicU64,
) {
    let next = Arc::clone(&shared.lock().unwrap_or_else(|error| error.into_inner()));
    if next.generation != context.generation
        || next.generation != routing_generation.load(Ordering::Acquire)
    {
        state.gestures.clear();
        state.last_joystick_sector = None;
    } else if next.controls != context.controls {
        state.gestures.clear();
    }
    *context = next;
}

fn input_worker(
    receiver: Receiver<DeviceEvent>,
    shared: Arc<Mutex<Arc<InputContext>>>,
    routing_generation: Arc<AtomicU64>,
    work: SyncSender<Work>,
    notices: Sender<String>,
    stopping: Arc<AtomicBool>,
) {
    let mut state = InputState::default();
    let mut context = Arc::clone(&shared.lock().unwrap_or_else(|error| error.into_inner()));
    while !stopping.load(Ordering::Acquire) {
        refresh_input_context(&shared, &mut context, &mut state, &routing_generation);
        handle_fired(
            &work,
            &context,
            &routing_generation,
            state.gestures.drain_due(Instant::now()),
        );
        let event = match state.gestures.next_deadline() {
            Some(deadline) => {
                match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match receiver.recv() {
                Ok(event) => Some(event),
                Err(_) => break,
            },
        };
        if let Some(event) = event {
            refresh_input_context(&shared, &mut context, &mut state, &routing_generation);
            handle_device_event(
                event,
                &mut state,
                &context,
                &routing_generation,
                &work,
                &notices,
            );
        }
    }
    state.gestures.clear();
}

fn send_lighting(device: &HidClient, state: &mut State) -> Result<()> {
    let slots_value: Vec<_> = slot_lighting(&state.slots, &state.agents, &state.config.lighting)
        .into_iter()
        .enumerate()
        .map(|(id, light)| json!({"id":id,"c":light.c,"b":light.b,"e":light.e,"s":light.s}))
        .collect();
    let mut aggregate = aggregate_lighting(&state.slots, &state.agents, &state.config.lighting);
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

fn close_device(device: &mut Option<HidClient>, state: &mut State, blank: bool) {
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
    state.last_focused_app.clear();
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
    device: Option<&HidClient>,
    state: &mut State,
    next: Option<String>,
) -> Result<()> {
    if state.selected_session == next {
        return Ok(());
    }
    reset_selected_route(state);
    state.selected_session = next;
    if let Some(device) = device {
        send_lighting(device, state)?;
    }
    log(state
        .selected_session
        .as_ref()
        .map(|session| format!("Herdr session selected: {session}"))
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
    let message = message.into();
    if state.last_routing_error != message {
        state.last_routing_error = message.clone();
        log(format!("Herdr session routing failed: {message}"));
    }
}

fn refresh_sessions(
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
    let selected = state.selected_session.as_deref();
    let previous_selected =
        selected.and_then(|name| state.sessions.iter().find(|session| session.name == name));
    let discovered_selected =
        selected.and_then(|name| discovered.iter().find(|session| session.name == name));
    let selected_changed = previous_selected != discovered_selected;
    if selected_changed {
        workers.clear();
        state.session_agents.clear();
        state.session_epochs.clear();
        state.session_slots.clear();
        if previous_selected.is_some() && discovered_selected.is_some() {
            reset_selected_route(state);
            if let Some(device) = device {
                send_lighting(device, state)?;
            }
        }
    }
    state.sessions = discovered;
    if state
        .selected_session
        .as_ref()
        .is_some_and(|name| !state.sessions.iter().any(|session| session.name == *name))
    {
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

fn sync_selected_worker(
    state: &mut State,
    workers: &mut HashMap<String, SessionWorker>,
    worker_generation: &mut u64,
    updates: &Sender<SessionUpdate>,
    stopping: &Arc<AtomicBool>,
) {
    workers.retain(|name, _| state.selected_session.as_deref() == Some(name));
    state
        .session_agents
        .retain(|name, _| state.selected_session.as_deref() == Some(name));
    state
        .session_epochs
        .retain(|name, _| state.selected_session.as_deref() == Some(name));
    state
        .session_slots
        .retain(|name, _| state.selected_session.as_deref() == Some(name));
    let Some(session) = state
        .selected_session
        .as_ref()
        .and_then(|name| state.sessions.iter().find(|session| session.name == *name))
    else {
        return;
    };
    if !workers.contains_key(&session.name) {
        *worker_generation += 1;
        workers.insert(
            session.name.clone(),
            spawn_session_worker(
                session.clone(),
                *worker_generation,
                updates.clone(),
                Arc::clone(stopping),
            ),
        );
    }
}

fn refresh_mappings(state: &mut State) {
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

fn refresh_routing(state: &mut State, device: Option<&HidClient>) -> Result<bool> {
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
    let Some(session) = state.selected_session.clone() else {
        return Ok(());
    };
    let (available, agents) = match state.session_agents.get(&session) {
        Some(agents) => (true, (*agents != state.agents).then(|| agents.clone())),
        None => (false, None),
    };
    if available && agents.is_none() {
        state.routing_ready = state.session_epochs.contains_key(&session);
        state.last_herdr_error.clear();
        return Ok(());
    }
    match agents {
        Some(agents) => {
            if agent_identity(state.agents.iter().find(|agent| agent.focused))
                != agent_identity(agents.iter().find(|agent| agent.focused))
            {
                state.invalidate_routing();
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

fn apply_session_update(
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
    if workers.get(session).map(|worker| worker.generation) != Some(generation) {
        return Ok(false);
    }
    match update {
        SessionUpdate::Agents {
            session,
            session_epoch,
            agents,
            ..
        } => {
            let replaced = state
                .session_epochs
                .get(&session)
                .is_some_and(|epoch| epoch != &session_epoch);
            if replaced {
                state.session_agents.remove(&session);
                state.session_slots.remove(&session);
                if state.selected_session.as_deref() == Some(&session) {
                    reset_selected_route(state);
                }
            }
            state.session_epochs.insert(session.clone(), session_epoch);
            state.session_agents.insert(session.clone(), agents);
            if session_route_is_verified(state, &session) {
                state.last_herdr_error.clear();
                apply_selected_agents(state, device)?;
            }
            Ok(false)
        }
        SessionUpdate::Unavailable { session, error, .. } => {
            let became_unavailable = state.session_agents.remove(&session).is_some();
            state.session_epochs.remove(&session);
            if state.selected_session.as_deref() == Some(&session) {
                apply_selected_agents(state, device)?;
                if state.last_herdr_error != error {
                    state.last_herdr_error = error.clone();
                    log(format!("Herdr session {session} unavailable: {error}"));
                }
            }
            Ok(became_unavailable)
        }
    }
}

fn session_route_is_verified(state: &State, session: &str) -> bool {
    state.routing_verified && state.selected_session.as_deref() == Some(session)
}

fn refresh_frontmost(state: &mut State) -> bool {
    match macos::frontmost_pid() {
        Ok(pid) if state.frontmost.as_ref().is_some_and(|app| app.pid == pid) => return false,
        Ok(_) => {}
        Err(error) => {
            let changed = state.frontmost.take().is_some();
            state.revoke_routing();
            if state.last_frontmost_error != error.to_string() {
                state.last_frontmost_error = error.to_string();
                log(format!("frontmost window unavailable: {error}"));
            }
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
            state.last_frontmost_error = error.to_string();
            log(format!("frontmost window unavailable: {error}"));
            changed
        }
        Err(_) => {
            let changed = state.frontmost.take().is_some();
            state.revoke_routing();
            changed
        }
    }
}

fn refresh_owner(state: &mut State, device: &mut Option<HidClient>) -> bool {
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

fn open_device(
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
            state.last_focused_app.clear();
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
            if state.last_device_error != message {
                state.last_device_error = message.clone();
                log(format!("device open failed: {message}"));
            }
        }
    }
    Ok(true)
}

fn publish(status: &Arc<Mutex<Value>>, state: &State) {
    *status.lock().unwrap_or_else(|error| error.into_inner()) = state.status();
}

fn update_input_context(context: &Mutex<Arc<InputContext>>, state: &State) {
    let target = state.agents.iter().find(|agent| agent.focused);
    let session = state
        .selected_session
        .as_ref()
        .and_then(|name| state.sessions.iter().find(|session| session.name == *name));
    *context.lock().unwrap_or_else(|error| error.into_inner()) = Arc::new(InputContext {
        controls: state.config.controls.clone(),
        effort: state.config.effort.clone(),
        session: session.cloned(),
        session_epoch: state
            .selected_session
            .as_ref()
            .and_then(|name| state.session_epochs.get(name))
            .cloned(),
        terminal: state.focused_terminal.clone(),
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
        routing_ready: state.routing_ready,
        generation: state.routing_generation(),
    });
}

fn handle_input_disconnect(
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

fn apply_config_load(
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

/// Run the bridge in the foreground.  `main`/the start action owns process
/// detachment; this function deliberately owns only the live daemon.
pub fn run_daemon() -> Result<()> {
    let config_path = config_path().map_err(|error| anyhow!(error))?;
    provision(&config_path).map_err(|error| anyhow!(error))?;
    let config = load(&config_path).map_err(|error| anyhow!(error))?;
    let startup_enabled_buttons = enabled_buttons(&config.controls);
    let (device_tx, device_rx) = mpsc::channel();
    let (input_notice_tx, input_notice_rx) = mpsc::channel();
    let (work_tx, work_rx) = mpsc::sync_channel(WORK_QUEUE_CAPACITY);
    let (session_update_tx, session_update_rx) = mpsc::channel();
    let stopping = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, Arc::clone(&stopping))?;
    signal_hook::flag::register(SIGTERM, Arc::clone(&stopping))?;
    let mut state = State::new(config);
    let input_context = Arc::new(Mutex::new(Arc::new(InputContext::new(&state.config))));
    let worker = thread::spawn({
        let routing_generation = Arc::clone(&state.routing_generation);
        let stopping = Arc::clone(&stopping);
        move || action_worker(work_rx, routing_generation, stopping)
    });
    let input = thread::spawn({
        let context = Arc::clone(&input_context);
        let routing_generation = Arc::clone(&state.routing_generation);
        let work = work_tx.clone();
        let stopping = Arc::clone(&stopping);
        move || {
            input_worker(
                device_rx,
                context,
                routing_generation,
                work,
                input_notice_tx,
                stopping,
            )
        }
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
        Arc::clone(&stopping),
    )?;
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let (control_result_tx, control_result_rx) = mpsc::sync_channel(1);
    let control_thread = thread::spawn(move || {
        let _ = control_result_tx.send(server.run_with_shutdown(&shutdown_rx));
    });
    let mut control_error = None;
    let mut device = None;
    let mut routing_due = Instant::now();
    let mut config_due = Instant::now();
    let mut sessions_due = Instant::now();
    let mut sessions_ready = true;
    let mut session_workers = HashMap::new();
    let mut worker_generation = 0;
    log("bridge started");
    while !stopping.load(Ordering::Acquire) {
        match control_result_rx.try_recv() {
            Ok(Ok(())) => {
                control_error = Some("Micro control server stopped unexpectedly".into());
                stopping.store(true, Ordering::Release);
                continue;
            }
            Ok(Err(error)) => {
                control_error = Some(format!("Micro control server failed: {error:#}"));
                stopping.store(true, Ordering::Release);
                continue;
            }
            Err(TryRecvError::Disconnected) => {
                control_error = Some("Micro control server thread disconnected".into());
                stopping.store(true, Ordering::Release);
                continue;
            }
            Err(TryRecvError::Empty) => {}
        }
        let mut changed = false;
        while let Ok(error) = input_notice_rx.try_recv() {
            changed |= handle_input_disconnect(&mut state, &mut device, error);
        }
        while let Ok(update) = session_update_rx.try_recv() {
            match apply_session_update(&mut state, update, &session_workers, device.as_ref()) {
                Ok(true) => {
                    sessions_due = sessions_due.min(Instant::now() + SESSION_RETRY_INTERVAL);
                }
                Ok(false) => {}
                Err(error) => log(format!("Herdr session update failed: {error:#}")),
            }
            changed = true;
        }
        let now = Instant::now();
        if now >= config_due {
            config_due = now + CONFIG_REFRESH_INTERVAL;
            changed |= apply_config_load(&mut state, load(&config_path), startup_enabled_buttons);
            if let Some(device) = device.as_ref() {
                match send_lighting(device, &mut state) {
                    Ok(()) => state.last_lighting_error.clear(),
                    Err(error) => {
                        let error = format!("{error:#}");
                        if state.last_lighting_error != error {
                            log(format!("lighting update failed: {error}"));
                            state.last_lighting_error = error;
                        }
                    }
                }
            }
        }
        if now >= sessions_due {
            sessions_due = now + SESSION_REFRESH_INTERVAL;
            if state.owner.is_none() {
                sessions_ready = match refresh_sessions(
                    &mut state,
                    device.as_ref(),
                    &mut session_workers,
                    &mut worker_generation,
                    &session_update_tx,
                    &stopping,
                ) {
                    Ok(shutdown) => {
                        if shutdown {
                            log("No Herdr sessions for 60 seconds; releasing device");
                            stopping.store(true, Ordering::Release);
                        }
                        true
                    }
                    Err(error) => {
                        log(format!("refresh failed: {error:#}"));
                        sessions_due = now + SESSION_RETRY_INTERVAL;
                        false
                    }
                };
            }
            changed = true;
        }
        if state.owner.is_none()
            && state
                .next_mapping_probe
                .is_some_and(|deadline| now >= deadline)
        {
            refresh_mappings(&mut state);
            changed = true;
        }
        if now >= routing_due {
            routing_due = now + ROUTING_REFRESH_INTERVAL;
            let input_generation = state.routing_generation();
            let input_ready = state.routing_ready;
            let previous_owner = state.owner.clone();
            changed |= refresh_frontmost(&mut state);
            changed |= refresh_owner(&mut state, &mut device);
            if previous_owner.is_some() && state.owner.is_none() {
                sessions_due = now;
            }
            if state.owner.is_none() && sessions_ready {
                match refresh_routing(&mut state, device.as_ref()) {
                    Ok(routing_changed) => changed |= routing_changed,
                    Err(error) => {
                        state.revoke_routing();
                        changed = true;
                        log(format!("refresh failed: {error:#}"));
                    }
                }
                sync_selected_worker(
                    &mut state,
                    &mut session_workers,
                    &mut worker_generation,
                    &session_update_tx,
                    &stopping,
                );
                match open_device(&mut device, &device_tx, &mut state) {
                    Ok(device_changed) => changed |= device_changed,
                    Err(error) => {
                        changed = true;
                        log(format!("refresh failed: {error:#}"));
                    }
                }
            }
            if state.owner.is_some() && state.device_restore_pending {
                match open_device(&mut device, &device_tx, &mut state) {
                    Ok(device_changed) => changed |= device_changed,
                    Err(error) => {
                        changed = true;
                        log(format!("native HID recovery failed: {error:#}"));
                    }
                }
            }
            changed |= input_generation != state.routing_generation()
                || input_ready != state.routing_ready;
        }
        if sessions_ready
            && state
                .frontmost
                .as_ref()
                .is_some_and(|frontmost| frontmost.process == GHOSTTY_PROCESS)
            && state.focused_terminal.as_ref().is_some_and(|terminal| {
                !state
                    .mappings
                    .iter()
                    .any(|mapping| mapping.terminal_id == *terminal)
            })
        {
            sessions_due = sessions_due.min(
                state
                    .next_mapping_probe
                    .unwrap_or(now + SESSION_RETRY_INTERVAL),
            );
        }
        if changed {
            update_input_context(&input_context, &state);
            publish(&status, &state);
        }
        let deadline = routing_due
            .min(config_due)
            .min(sessions_due)
            .min(state.next_mapping_probe.unwrap_or(sessions_due));
        match input_notice_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(error) => {
                if handle_input_disconnect(&mut state, &mut device, error) {
                    update_input_context(&input_context, &state);
                    publish(&status, &state);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    log("stopping");
    state.revoke_routing();
    stopping.store(true, Ordering::Release);
    close_device(&mut device, &mut state, true);
    drop(device_tx);
    let _ = input.join();
    publish(&status, &state);
    drop(work_tx);
    let _ = worker.join();
    let _ = shutdown_tx.send(());
    let _ = control_thread.join();
    match control_error {
        Some(error) => Err(anyhow!(error)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
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
    fn timestamps_are_utc_iso_8601() {
        assert_eq!(format_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            format_timestamp(UNIX_EPOCH + Duration::from_millis(946_782_245_678)),
            "2000-01-02T03:04:05.678Z"
        );
    }

    #[test]
    fn status_matches_the_control_contract() {
        let mut state = State::new(Config::default());
        state.selected_session = Some("work".into());
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
    fn key_events_capture_the_ready_session_without_hardware() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let target = agent("terminal", "pane", "codex");
        let config = Config::default();
        let context = InputContext {
            controls: config.controls,
            effort: config.effort,
            session: Some(session("work")),
            session_epoch: Some("epoch".into()),
            terminal: Some("terminal".into()),
            target: Some(target),
            slots: vec![None; SLOT_COUNT],
            routing_ready: true,
            generation: 0,
        };
        let routing_generation = AtomicU64::new(0);
        let mut state = InputState::default();
        let (notices, _) = mpsc::channel();
        handle_device_event(
            DeviceEvent::Key {
                key: "ACT09".into(),
                action: 1,
            },
            &mut state,
            &context,
            &routing_generation,
            &sender,
            &notices,
        );
        match receiver.recv_timeout(Duration::from_millis(100)).unwrap() {
            Work::Binding {
                session, target, ..
            } => {
                assert_eq!(session.name, "work");
                assert_eq!(
                    target,
                    Some(("terminal".into(), "pane".into(), Some("codex".into())))
                );
            }
            _ => panic!("expected configured binding"),
        }
    }

    #[test]
    fn oai_agent_keys_map_to_slots() {
        assert_eq!(agent_slot("AG00"), Some(0));
        assert_eq!(agent_slot("AG05"), Some(5));
        assert_eq!(agent_slot("AG06"), None);
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
        state.selected_session = Some("work".into());
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
        state.selected_session = Some("work".into());
        state
            .session_agents
            .insert("work".into(), vec![agent("terminal", "pane", "codex")]);

        apply_selected_agents(&mut state, None).unwrap();
        assert!(state.routing_ready);
        state.session_agents.clear();
        apply_selected_agents(&mut state, None).unwrap();
        assert!(!state.routing_ready);
        assert!(state.agents.is_empty());
    }

    #[test]
    fn agent_updates_require_a_verified_terminal_route() {
        let mut state = State::new(Config::default());
        state.selected_session = Some("work".into());
        state.focused_terminal = Some("terminal".into());
        state.frontmost = Some(macos::Frontmost {
            app_name: "Ghostty".into(),
            process: GHOSTTY_PROCESS.into(),
            pid: 1,
            title: String::new(),
        });
        let stopping = Arc::new(AtomicBool::new(true));
        let (updates, _) = mpsc::channel();
        let worker = spawn_session_worker(session("work"), 7, updates, stopping);
        let workers = HashMap::from([("work".into(), worker)]);
        let update = || SessionUpdate::Agents {
            session: "work".into(),
            generation: 7,
            session_epoch: "epoch".into(),
            agents: vec![agent("terminal", "pane", "codex")],
        };

        apply_session_update(&mut state, update(), &workers, None).unwrap();
        assert!(!state.routing_ready);

        state.routing_verified = true;
        apply_session_update(&mut state, update(), &workers, None).unwrap();
        assert!(state.routing_ready);
    }

    #[test]
    fn live_session_unavailability_requests_one_discovery() {
        let mut state = State::new(Config::default());
        state.session_agents.insert("work".into(), Vec::new());
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
            title: String::new(),
        });
        let mut device = None;

        assert!(refresh_owner(&mut state, &mut device));
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
