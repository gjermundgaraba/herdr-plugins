//! Input dispatch and action execution: device events become queued work,
//! and the action worker executes it against the routed Herdr session.

use anyhow::{Context, Result, anyhow, bail};
use codex_micro::DeviceEvent;
use herdr_client::{AgentInfo, Client, SessionSnapshot};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use crate::{
    actions::{
        GHOSTTY_PROCESS, focus_agent, focus_pane, open_diff, prompt as send_prompt, scroll_plan,
        submit,
    },
    config::{
        Action, Binding, Direction, Modifier, VerticalDirection, key_action_code, key_binding,
    },
    gestures::{Fired, GestureContext, GestureDispatcher},
    ghostty::{focused_terminal_id, scroll_terminal},
    herdr::{COMMAND_TIMEOUT, Session, current_snapshot, run_command_with_timeout},
    macos,
    protocol::{SLOT_COUNT, joystick_event},
    setup::plugin_root,
};

use super::{
    log,
    reconcile::{InputContext, InputRoute},
};

#[derive(Default)]
struct InputState {
    gestures: GestureDispatcher,
    last_joystick_sector: Option<u8>,
}

pub(super) struct Work {
    source: String,
    session: Session,
    terminal: String,
    generation: u64,
    kind: WorkKind,
}

enum WorkKind {
    Binding {
        binding: Box<Binding>,
        target: Option<(String, String, Option<String>)>,
    },
    FocusSlot {
        pane_id: String,
        agent_terminal_id: String,
    },
}

fn require_agent(agent: Option<&AgentInfo>) -> Result<&AgentInfo> {
    agent.ok_or_else(|| anyhow!("no focused Herdr agent"))
}

pub(super) fn agent_identity(
    agent: Option<&AgentInfo>,
) -> Option<(String, String, Option<String>)> {
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
        Action::Script { .. } => "script",
        Action::FocusPane { .. } => "focus-pane",
        Action::Scroll { .. } => "scroll",
        Action::Key { .. } => "key",
    }
}

struct DispatchLease<'a> {
    session: &'a Session,
    terminal: &'a str,
    generation: u64,
    routing_generation: &'a AtomicU64,
}

impl DispatchLease<'_> {
    fn client(&self) -> Client {
        self.session.client()
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
    let plan = scroll_plan(pane, layout, direction.as_str(), percent)?;
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
        direction.as_str(),
        lease.session.name,
        plan.pane_id
    ));
    Ok(true)
}

fn script_command(
    command: &str,
    args: &[String],
    root: &Path,
    session: &Session,
    pane: &str,
) -> Command {
    let mut child = Command::new(command);
    child
        .args(args)
        .current_dir(root)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("HERDR_ACTIVE_PANE_ID")
        .env_remove("HERDR_ACTIVE_TAB_ID")
        .env_remove("HERDR_ACTIVE_WORKSPACE_ID")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
        .env_remove("HERDR_TAB_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", &session.socket_path)
        .env("HERDR_SESSION", &session.name)
        .env("HERDR_PANE_ID", pane);
    child
}

fn execute_script(
    command: &str,
    args: &[String],
    current: Option<&AgentInfo>,
    lease: &DispatchLease<'_>,
) -> Result<()> {
    let current = require_agent(current)?;
    lease.ensure()?;
    let mut child = script_command(
        command,
        args,
        &plugin_root()?,
        lease.session,
        &current.pane_id,
    );
    run_command_with_timeout(&mut child, COMMAND_TIMEOUT)?;
    Ok(())
}

fn execute_action(
    action: &Action,
    current: Option<&AgentInfo>,
    snapshot: &SessionSnapshot,
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
        Action::Script { command, args } => execute_script(command, args, current, lease)?,
        Action::FocusPane { direction } => {
            let direction = direction.as_str();
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

fn execute_work(work: Work, routing_generation: &AtomicU64, stopping: &AtomicBool) -> Result<()> {
    let Work {
        source,
        session,
        terminal,
        generation,
        kind,
    } = work;
    if generation != routing_generation.load(Ordering::Acquire) {
        log("control ignored: stale Herdr routing");
        return Ok(());
    }
    let snapshot = match current_snapshot(&session.client()) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log(format!(
                "control failed: refresh {}: {error:#}",
                session.name
            ));
            return Ok(());
        }
    };
    if generation != routing_generation.load(Ordering::Acquire) {
        log("control ignored: stale Herdr routing");
        return Ok(());
    }
    // A stop between the snapshot and the action must not start the action.
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let lease = DispatchLease {
        session: &session,
        terminal: &terminal,
        generation,
        routing_generation,
    };
    match kind {
        WorkKind::FocusSlot {
            pane_id,
            agent_terminal_id,
        } => {
            if !snapshot
                .agents
                .iter()
                .any(|agent| agent.pane_id == pane_id && agent.terminal_id == agent_terminal_id)
            {
                bail!("Agent slot pane disappeared");
            }
            lease.ensure()?;
            focus_agent(&lease.client(), &pane_id)?;
            log(format!("{source}: focused {}/{pane_id}", session.name));
        }
        WorkKind::Binding { binding, target } => {
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
            let executed = execute_action(&action, current, &snapshot, &lease)
                .with_context(|| format!("{source}: {}", action_name(&action)))?;
            if executed {
                log(format!(
                    "{source}: {} in {}{}",
                    action_name(&action),
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
        }
    }
    Ok(())
}

fn queue_work(sender: &SyncSender<Work>, work: Work) {
    if let Err(error) = sender.try_send(work) {
        log(match error {
            TrySendError::Full(_) => "control ignored: action queue full",
            TrySendError::Disconnected(_) => "control ignored: worker stopped",
        });
    }
}

pub(super) fn action_worker(
    receiver: Receiver<Work>,
    routing_generation: Arc<AtomicU64>,
    stopping: Arc<AtomicBool>,
) {
    while !stopping.load(Ordering::Acquire) {
        let work = match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(work) => work,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if stopping.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) = execute_work(work, &routing_generation, &stopping) {
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
    route: Option<InputRoute>,
    target: Option<AgentInfo>,
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
        Some(InputRoute {
            session,
            terminal,
            generation,
        }) => {
            queue_work(
                sender,
                Work {
                    source,
                    session,
                    terminal,
                    generation,
                    kind: WorkKind::Binding {
                        binding: Box::new(binding),
                        target: agent_identity(target.as_ref()),
                    },
                },
            );
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
        if fired
            .context
            .as_ref()
            .is_some_and(|context| context.generation != routing_generation.load(Ordering::Acquire))
        {
            log(format!("{} ignored: stale Herdr routing", fired.source));
            continue;
        }
        let route = context.selected(routing_generation).filter(|route| {
            fired
                .context
                .as_ref()
                .is_some_and(|context| context.session == route.session.name)
        });
        queue_binding(
            sender,
            Binding::Action(fired.action),
            fired.source,
            route,
            context.target.clone(),
        );
    }
}

fn agent_slot(key: &str) -> Option<usize> {
    key.strip_prefix("AG0")
        .and_then(|index| index.parse::<usize>().ok())
        .filter(|index| *index < SLOT_COUNT)
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
            let (binding, direction) = match next.direction {
                Some(Direction::Up) => (&controls.joystick.up, Direction::Up),
                Some(Direction::Down) => (&controls.joystick.down, Direction::Down),
                Some(Direction::Left) => (&controls.joystick.left, Direction::Left),
                Some(Direction::Right) => (&controls.joystick.right, Direction::Right),
                None => return,
            };
            if let Some(binding) = binding.clone() {
                queue_binding(
                    worker,
                    binding,
                    format!("joystick {}", direction.as_str()),
                    context.selected(routing_generation),
                    context.target.clone(),
                );
            }
        }
        DeviceEvent::Key { key, action } => {
            if let Some(index) = agent_slot(&key) {
                if action == 1 {
                    let session = context.selected(routing_generation);
                    let agent = context.slots[index].as_ref();
                    match (session, agent) {
                        (Some(route), Some(agent)) => {
                            queue_work(
                                worker,
                                Work {
                                    source: key,
                                    session: route.session,
                                    terminal: route.terminal,
                                    generation: route.generation,
                                    kind: WorkKind::FocusSlot {
                                        pane_id: agent.pane_id.clone(),
                                        agent_terminal_id: agent.terminal_id.clone(),
                                    },
                                },
                            );
                        }
                        _ => log(format!("{key} ignored: no ready Agent slot")),
                    }
                }
                return;
            }
            let binding = key_binding(controls, &key, action);
            let captured = context.selected(routing_generation);
            let gesture_context = captured.as_ref().map(|route| GestureContext {
                session: route.session.name.clone(),
                generation: route.generation,
            });
            if matches!(key.as_str(), "ENC_CC" | "ENC_CW") {
                if let Some(binding) = binding {
                    queue_binding(worker, binding, key, captured, context.target.clone());
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
                        context.target.clone(),
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
    let next_generation = next.route.as_ref().map(|route| route.generation);
    if next_generation != context.route.as_ref().map(|route| route.generation)
        || next_generation
            .is_some_and(|generation| generation != routing_generation.load(Ordering::Acquire))
    {
        state.gestures.clear();
        state.last_joystick_sector = None;
    } else if next.controls != context.controls {
        state.gestures.clear();
    }
    *context = next;
}

pub(super) fn input_worker(
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

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::mpsc};

    use super::*;
    use crate::config::Config;

    fn temp_dir(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "herdr-micro-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

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
    fn script_context_drives_the_bundled_adapter() {
        let dir = temp_dir("script-context");
        let fake_herdr = dir.join("herdr");
        let calls = dir.join("calls");
        let environment = dir.join("environment");
        fs::write(
            &fake_herdr,
            r#"#!/bin/sh
if [ ! -e "$HERDR_TEST_ENV" ]; then
    printf '%s\n' "$HERDR_SESSION" "$HERDR_SOCKET_PATH" "$HERDR_PANE_ID" "$PWD" > "$HERDR_TEST_ENV"
fi
printf '%s\n' --call "$@" >> "$HERDR_TEST_LOG"
"#,
        )
        .unwrap();
        fs::set_permissions(&fake_herdr, fs::Permissions::from_mode(0o700)).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = Config::default();
        let target_session = session("work");

        let Action::Script { command, args } = key_binding(&config.controls, "ENC_CC", 1)
            .unwrap()
            .resolve("codex")
            .unwrap()
        else {
            panic!("expected Codex script action");
        };
        let mut child = script_command(&command, &args, root, &target_session, "w9:p4");
        child
            .env("HERDR_BIN_PATH", &fake_herdr)
            .env("HERDR_TEST_LOG", &calls)
            .env("HERDR_TEST_ENV", &environment);
        run_command_with_timeout(&mut child, COMMAND_TIMEOUT).unwrap();
        assert_eq!(
            fs::read_to_string(&environment).unwrap(),
            format!("work\n/tmp/work.sock\nw9:p4\n{}\n", root.display())
        );
        assert_eq!(
            fs::read_to_string(&calls).unwrap(),
            "--call\npane\nsend-keys\nw9:p4\nalt+.\n"
        );

        fs::remove_file(&calls).unwrap();
        let Action::Script { command, args } = key_binding(&config.controls, "ENC_CW", 1)
            .unwrap()
            .resolve("claude")
            .unwrap()
        else {
            panic!("expected Claude script action");
        };
        let mut child = script_command(&command, &args, root, &target_session, "w9:p4");
        child
            .env("HERDR_BIN_PATH", &fake_herdr)
            .env("HERDR_TEST_LOG", &calls)
            .env("HERDR_TEST_ENV", &environment);
        run_command_with_timeout(&mut child, COMMAND_TIMEOUT).unwrap();
        assert_eq!(
            fs::read_to_string(&calls).unwrap(),
            "--call\npane\nsend-text\nw9:p4\n/effort\n--call\npane\nsend-keys\nw9:p4\nenter\n--call\npane\nsend-keys\nw9:p4\nleft\n--call\npane\nsend-keys\nw9:p4\nenter\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scripts_require_a_focused_agent() {
        let routing_generation = AtomicU64::new(3);
        let session = session("work");
        let lease = DispatchLease {
            session: &session,
            terminal: "terminal",
            generation: 3,
            routing_generation: &routing_generation,
        };
        let error = execute_script("/usr/bin/true", &[], None, &lease).unwrap_err();
        assert_eq!(error.to_string(), "no focused Herdr agent");
    }

    #[test]
    fn key_events_capture_the_ready_session_without_hardware() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let target = agent("terminal", "pane", "codex");
        let config = Config::default();
        let context = InputContext {
            controls: config.controls,
            route: Some(InputRoute {
                session: session("work"),
                terminal: "terminal".into(),
                generation: 0,
            }),
            target: Some(target),
            slots: vec![None; SLOT_COUNT],
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
            Work {
                session,
                kind: WorkKind::Binding { target, .. },
                ..
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
    fn stale_gesture_is_rejected_after_same_session_generation_change() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let config = Config::default();
        let context = InputContext {
            controls: config.controls,
            route: Some(InputRoute {
                session: session("work"),
                terminal: "terminal".into(),
                generation: 2,
            }),
            target: None,
            slots: vec![None; SLOT_COUNT],
        };
        let routing_generation = AtomicU64::new(2);

        handle_fired(
            &sender,
            &context,
            &routing_generation,
            vec![Fired {
                action: Action::Submit,
                source: "ACT00 tap".into(),
                context: Some(GestureContext {
                    session: "work".into(),
                    generation: 1,
                }),
            }],
        );

        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn oai_agent_keys_map_to_slots() {
        assert_eq!(agent_slot("AG00"), Some(0));
        assert_eq!(agent_slot("AG05"), Some(5));
        assert_eq!(agent_slot("AG06"), None);
    }
}
