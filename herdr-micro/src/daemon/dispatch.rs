//! Input dispatch and action execution: device events become queued work,
//! and the action worker executes it against the routed Herdr session.

use anyhow::{Context, Result, anyhow, bail};
use codex_micro::DeviceEvent;
use herdr_hub_client::{AgentInfo, AgentStatus, HubClient};
use serde::Deserialize;
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
    actions::{Caller, focus_agent, focus_pane, open_diff, prompt as send_prompt, submit},
    config::{Action, Binding, Direction, Modifier, key_action_code, key_binding},
    gestures::{Fired, GestureContext, GestureDispatcher},
    hub::Session,
    macos,
    process::{COMMAND_TIMEOUT, run_command_with_timeout},
    protocol::{SLOT_COUNT, joystick_event},
    setup::plugin_root,
};

use super::{
    RuntimeEvent, log,
    reconcile::{ActiveRoute, InputContext, InputRoute},
};

#[derive(Default)]
struct InputState {
    gestures: GestureDispatcher,
    last_joystick_sector: Option<u8>,
}

pub(super) struct Work {
    source: String,
    session: Session,
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

/// Match Herdr's own prompt gate: only a blocked agent refuses text.
fn ready_agent(agent: Option<&AgentInfo>) -> Result<&AgentInfo> {
    let agent = require_agent(agent)?;
    if agent.agent_status.as_str() == AgentStatus::BLOCKED {
        bail!("focused agent is blocked")
    }
    Ok(agent)
}

fn action_name(action: &Action) -> &'static str {
    match action {
        Action::Prompt { .. } => "prompt",
        Action::Diff => "diff",
        Action::Fast => "fast",
        Action::Submit => "submit",
        Action::Script { .. } => "script",
        Action::FocusPane { .. } => "focus-pane",
        Action::Key { .. } => "key",
    }
}

struct DispatchLease<'a> {
    session: &'a Session,
    generation: u64,
    routing_generation: &'a AtomicU64,
    active_route: &'a Mutex<ActiveRoute>,
    stopping: &'a AtomicBool,
}

impl DispatchLease<'_> {
    fn ensure(&self) -> Result<()> {
        if self.stopping.load(Ordering::Acquire) {
            bail!("Micro bridge is stopping")
        }
        let active = self
            .active_route
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if active.session_key.as_deref() != Some(self.session.key.as_str()) {
            bail!("Herdr session is no longer active")
        }
        if self.generation != self.routing_generation.load(Ordering::Acquire) {
            bail!("stale Herdr routing")
        }
        if self.stopping.load(Ordering::Acquire) {
            bail!("Micro bridge is stopping")
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct AgentResult {
    agent: AgentInfo,
}

fn get_agent(call: &Caller<'_>, pane_id: &str) -> Result<AgentInfo> {
    Ok(
        serde_json::from_value::<AgentResult>(call("agent.get", json!({ "target": pane_id }))?)?
            .agent,
    )
}

fn revalidate_binding(
    call: &Caller<'_>,
    target: &Option<(String, String, Option<String>)>,
) -> Option<AgentInfo> {
    let (_, pane_id, _) = target.as_ref()?;
    let agent = get_agent(call, pane_id).ok()?;
    (agent.focused && agent_identity(Some(&agent)).as_ref() == target.as_ref()).then_some(agent)
}

fn script_command(
    command: &str,
    args: &[String],
    root: &Path,
    session: &Session,
    pane: &str,
) -> Result<Command> {
    let socket_path = session
        .socket_path
        .as_ref()
        .ok_or_else(|| anyhow!("script actions require a local Herdr session"))?;
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
        .env("HERDR_SOCKET_PATH", socket_path)
        .env("HERDR_SESSION", &session.name)
        .env("HERDR_PANE_ID", pane);
    Ok(child)
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
    )?;
    run_command_with_timeout(&mut child, COMMAND_TIMEOUT)?;
    Ok(())
}

fn execute_action(
    action: &Action,
    current: Option<&AgentInfo>,
    call: &Caller<'_>,
    lease: &DispatchLease<'_>,
) -> Result<()> {
    match action {
        Action::Prompt { prompt, submit } => {
            let current = ready_agent(current)?;
            lease.ensure()?;
            send_prompt(call, prompt, submit.unwrap_or(true), current)?;
        }
        Action::Diff => {
            let current = require_agent(current)?;
            lease.ensure()?;
            open_diff(call, current)?;
        }
        Action::Fast => {
            let current = ready_agent(current)?;
            match current.agent.as_deref().unwrap_or_default() {
                "codex" => {
                    lease.ensure()?;
                    send_prompt(call, "/fast", true, current)?;
                }
                "pi" => {
                    lease.ensure()?;
                    send_prompt(call, "/fast", false, current)?;
                    lease.ensure()?;
                    submit(call, current)?;
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
            submit(call, current)?;
        }
        Action::Script { command, args } => execute_script(command, args, current, lease)?,
        Action::FocusPane { direction } => {
            let direction = direction.as_str();
            let pane = require_agent(current)?.pane_id.clone();
            lease.ensure()?;
            focus_pane(call, &pane, direction)?;
            log(format!(
                "joystick focus {direction}: {}/{pane}",
                lease.session.name
            ));
        }
        // Key actions cannot reach the worker: top-level keys execute inline in
        // queue_binding and byAgent keys are rejected at parse.
        Action::Key { .. } => bail!("key action routed to the action worker"),
    }
    Ok(())
}

fn execute_work(
    work: Work,
    routing_generation: &AtomicU64,
    active_route: &Mutex<ActiveRoute>,
    stopping: &AtomicBool,
    client: &HubClient,
) -> Result<()> {
    let Work {
        source,
        session,
        generation,
        kind,
    } = work;
    if generation != routing_generation.load(Ordering::Acquire) {
        log("control ignored: stale Herdr routing");
        return Ok(());
    }
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let call = |method: &str, params: Value| {
        client
            .call(&session.key, method, params, COMMAND_TIMEOUT)
            .map_err(anyhow::Error::from)
    };
    let call: &Caller<'_> = &call;
    let lease = DispatchLease {
        session: &session,
        generation,
        routing_generation,
        active_route,
        stopping,
    };
    match kind {
        WorkKind::FocusSlot {
            pane_id,
            agent_terminal_id,
        } => {
            let agent = get_agent(call, &pane_id).context("Agent slot pane disappeared")?;
            if agent.pane_id != pane_id || agent.terminal_id != agent_terminal_id {
                bail!("Agent slot pane disappeared");
            }
            lease.ensure()?;
            focus_agent(call, &pane_id)?;
            log(format!("{source}: focused {}/{pane_id}", session.name));
        }
        WorkKind::Binding { binding, target } => {
            let Some(current) = revalidate_binding(call, &target) else {
                log(format!("{source} ignored: focused pane changed"));
                return Ok(());
            };
            let Some(action) = binding.resolve(current.agent.as_deref().unwrap_or("")) else {
                return Ok(());
            };
            execute_action(&action, Some(&current), call, &lease)
                .with_context(|| format!("{source}: {}", action_name(&action)))?;
            log(format!(
                "{source}: {} in {} for {} in {}",
                action_name(&action),
                session.name,
                current.agent.as_deref().unwrap_or("unknown"),
                current.pane_id
            ));
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
    active_route: Arc<Mutex<ActiveRoute>>,
    stopping: Arc<AtomicBool>,
) {
    let client = HubClient::new();
    while !stopping.load(Ordering::Acquire) {
        let work = match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(work) => work,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if stopping.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) =
            execute_work(work, &routing_generation, &active_route, &stopping, &client)
        {
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
            generation,
        }) => {
            queue_work(
                sender,
                Work {
                    source,
                    session,
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
                .is_some_and(|context| context.session_key == route.session.key)
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
    notices: &Sender<RuntimeEvent>,
) {
    let controls = &context.controls;
    match event {
        DeviceEvent::Disconnected { error } => {
            state.gestures.clear();
            state.last_joystick_sector = None;
            let _ = notices.send(RuntimeEvent::InputDisconnected(error));
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
                session_key: route.session.key.clone(),
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
    notices: Sender<RuntimeEvent>,
    stopping: Arc<AtomicBool>,
) {
    let mut state = InputState::default();
    let mut context = Arc::clone(&shared.lock().unwrap_or_else(|error| error.into_inner()));
    while !stopping.load(Ordering::Acquire) {
        refresh_input_context(&shared, &mut context, &mut state, &routing_generation);
        if stopping.load(Ordering::Acquire) {
            break;
        }
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
            if stopping.load(Ordering::Acquire) {
                break;
            }
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
    use std::{cell::RefCell, fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::mpsc};

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
            key: format!("local/{name}"),
            name: name.into(),
            socket_path: Some(format!("/tmp/{name}.sock").into()),
        }
    }

    fn active_route(session: &Session) -> Mutex<ActiveRoute> {
        Mutex::new(ActiveRoute {
            session_key: Some(session.key.clone()),
        })
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
        let mut child = script_command(&command, &args, root, &target_session, "w9:p4").unwrap();
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
        let mut child = script_command(&command, &args, root, &target_session, "w9:p4").unwrap();
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
        let active_route = active_route(&session);
        let lease = DispatchLease {
            session: &session,
            generation: 3,
            routing_generation: &routing_generation,
            active_route: &active_route,
            stopping: &AtomicBool::new(false),
        };
        let error = execute_script("/usr/bin/true", &[], None, &lease).unwrap_err();
        assert_eq!(error.to_string(), "no focused Herdr agent");
    }

    #[test]
    fn scripts_refuse_sessions_without_a_local_socket() {
        let session = Session {
            key: "remote/work".into(),
            name: "work".into(),
            socket_path: None,
        };
        let error =
            script_command("/usr/bin/true", &[], Path::new("/tmp"), &session, "w1:p1").unwrap_err();
        assert_eq!(
            error.to_string(),
            "script actions require a local Herdr session"
        );
    }

    #[test]
    fn binding_revalidation_requires_identity_and_focus() {
        let expected = agent("terminal", "pane", "codex");
        let target = agent_identity(Some(&expected));
        let calls = RefCell::new(Vec::new());
        let reply = RefCell::new(expected.clone());
        let caller = |method: &str, params: Value| {
            calls.borrow_mut().push((method.to_owned(), params));
            Ok(json!({"type":"agent_info", "agent": reply.borrow().clone()}))
        };

        assert_eq!(
            revalidate_binding(&caller, &target).map(|agent| agent.pane_id),
            Some("pane".into())
        );
        reply.borrow_mut().focused = false;
        assert!(revalidate_binding(&caller, &target).is_none());
        reply.borrow_mut().focused = true;
        reply.borrow_mut().terminal_id = "replacement".into();
        assert!(revalidate_binding(&caller, &target).is_none());
        assert_eq!(
            calls.borrow()[0],
            ("agent.get".into(), json!({"target":"pane"}))
        );
    }

    #[test]
    fn stop_after_revalidation_prevents_the_action() {
        let expected = agent("terminal", "pane", "codex");
        let target = agent_identity(Some(&expected));
        let stopping = AtomicBool::new(false);
        let calls = RefCell::new(Vec::new());
        let caller = |method: &str, params: Value| {
            calls.borrow_mut().push((method.to_owned(), params));
            stopping.store(true, Ordering::Release);
            Ok(json!({"type":"agent_info", "agent": expected}))
        };
        let current = revalidate_binding(&caller, &target).unwrap();
        let routing_generation = AtomicU64::new(3);
        let session = session("work");
        let active_route = active_route(&session);
        let lease = DispatchLease {
            session: &session,
            generation: 3,
            routing_generation: &routing_generation,
            active_route: &active_route,
            stopping: &stopping,
        };

        let error = execute_action(&Action::Submit, Some(&current), &caller, &lease).unwrap_err();

        assert_eq!(error.to_string(), "Micro bridge is stopping");
        assert_eq!(calls.borrow().len(), 1, "submit RPC must not start");
    }

    #[test]
    fn lease_rejects_active_session_changes() {
        let routing_generation = AtomicU64::new(3);
        let stopping = AtomicBool::new(false);
        let session = session("work");
        let active_route = active_route(&session);
        let lease = DispatchLease {
            session: &session,
            generation: 3,
            routing_generation: &routing_generation,
            active_route: &active_route,
            stopping: &stopping,
        };
        lease.ensure().unwrap();

        let mut active = active_route.lock().unwrap();
        active.session_key = Some("local/personal".into());
        drop(active);
        assert_eq!(
            lease.ensure().unwrap_err().to_string(),
            "Herdr session is no longer active"
        );
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
                    session_key: "local/work".into(),
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
