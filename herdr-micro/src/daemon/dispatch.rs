//! Input dispatch and action execution: device events become queued work,
//! and the action worker executes it through the captured Herdr client route.

use anyhow::{Context, Result, anyhow, bail};
use codex_micro::DeviceEvent;
use herdr_client::AgentStatus;
use herdr_frontend::{Agent, Input, NavigationTarget};
use std::{
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use crate::{
    actions::{Call, Caller, focus_pane, prompt as send_prompt, submit},
    config::{Action, Binding, Direction, Modifier, key_action_code, key_binding},
    frontends::ClientRoute,
    gestures::{Fired, GestureContext, GestureDispatcher},
    macos,
    process::{COMMAND_TIMEOUT, run_command_with_timeout},
    protocol::{SLOT_COUNT, joystick_event},
    setup::plugin_root,
};

use super::{RuntimeEvent, log, reconcile::InputContext};

#[derive(Default)]
struct InputState {
    gestures: GestureDispatcher,
    last_joystick_sector: Option<u8>,
}

pub(super) struct Work {
    source: String,
    route: ClientRoute,
    kind: WorkKind,
}

enum WorkKind {
    Binding {
        binding: Box<Binding>,
        target: Option<Box<Agent>>,
    },
    FocusSlot {
        pane_id: String,
    },
}

/// Match Herdr's own prompt gate: only a blocked agent refuses text.
fn ready_agent(agent: &Agent) -> Result<&Agent> {
    if agent.agent_status.as_str() == AgentStatus::BLOCKED {
        bail!("focused agent is blocked")
    }
    Ok(agent)
}

fn action_name(action: &Action) -> &'static str {
    match action {
        Action::Prompt { .. } => "prompt",
        Action::Input { .. } => "input",
        Action::Fast => "fast",
        Action::Submit => "submit",
        Action::Script { .. } => "script",
        Action::FocusPane { .. } => "focus-pane",
        Action::Key { .. } => "key",
    }
}

fn script_command(
    command: &str,
    args: &[String],
    root: &Path,
    route: &ClientRoute,
    pane: &str,
) -> Result<Command> {
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
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_BIN_PATH")
        .env("HERDR_MICRO_BIN_PATH", root.join("bin/herdr-micro"))
        .env("HERDR_FRONTEND_SOCKET", &route.socket_path)
        .env("HERDR_PANE_ID", pane);
    Ok(child)
}

fn execute_script(
    command: &str,
    args: &[String],
    current: &Agent,
    route: &ClientRoute,
) -> Result<()> {
    let mut child = script_command(command, args, &plugin_root()?, route, &current.pane_id)?;
    run_command_with_timeout(&mut child, COMMAND_TIMEOUT)
}

fn execute_action(
    action: &Action,
    current: &Agent,
    call: &Caller<'_>,
    route: &ClientRoute,
) -> Result<()> {
    match action {
        Action::Input { text, keys } => {
            ordinary_input(call, text, keys)?;
        }
        Action::Prompt { prompt, submit } => {
            let current = ready_agent(current)?;
            send_prompt(call, prompt, submit.unwrap_or(true), current)?;
        }
        Action::Fast => {
            let current = ready_agent(current)?;
            match current.agent.as_deref().unwrap_or_default() {
                "codex" => {
                    send_prompt(call, "/fast", true, current)?;
                }
                "pi" => {
                    send_prompt(call, "/fast", false, current)?;
                    submit(call)?;
                }
                other => bail!(
                    "unsupported focused agent: {}",
                    if other.is_empty() { "none" } else { other }
                ),
            }
        }
        Action::Submit => {
            submit(call)?;
        }
        Action::Script { command, args } => execute_script(command, args, current, route)?,
        Action::FocusPane { direction } => {
            let direction = direction.as_str();
            let pane = &current.pane_id;
            focus_pane(call, pane, direction)?;
            log(format!(
                "joystick focus {direction}: {}/{pane}",
                route.client_id
            ));
        }
        // Key actions cannot reach the worker: top-level keys execute inline in
        // queue_binding and byAgent keys are rejected at parse.
        Action::Key { .. } => bail!("key action routed to the action worker"),
    }
    Ok(())
}

fn ordinary_input(
    call: &Caller<'_>,
    text: &Option<String>,
    keys: &Option<Vec<String>>,
) -> Result<()> {
    let input = match (text, keys) {
        (Some(text), None) => Input::Text(text.clone()),
        (None, Some(keys)) => Input::Keys(keys.clone()),
        _ => bail!("input requires exactly one of text or keys"),
    };
    call(Call::Input(input))
}

fn execute_work(
    work: Work,
    stopping: &AtomicBool,
    context: &Mutex<Arc<InputContext>>,
) -> Result<()> {
    let client = work.route.client().with_timeout(COMMAND_TIMEOUT);
    let transport = |call: Call| {
        match call {
            Call::Navigate { pane_id } => {
                client.navigate(&work.route.wire(), &NavigationTarget::Pane(pane_id))?;
            }
            Call::Input(input) => {
                client.input(&input)?;
            }
            // The TUI rejects calls on an inactive endpoint; no pre-check here.
            Call::Call { method, params } => {
                client.call(&work.route.wire(), &method, params)?;
            }
        }
        Ok(())
    };
    run_work(&work, stopping, context, &transport)
}

fn selected_client(context: &Mutex<Arc<InputContext>>, route: &ClientRoute) -> bool {
    let current = context.lock().unwrap_or_else(|e| e.into_inner());
    current
        .route
        .as_ref()
        .is_some_and(|r| r.socket_path == route.socket_path)
}

/// Execute captured work, checking before every call that the bridge is
/// still running and the captured TUI is still the uniquely focused client.
fn run_work(
    work: &Work,
    stopping: &AtomicBool,
    context: &Mutex<Arc<InputContext>>,
    transport: &Caller<'_>,
) -> Result<()> {
    let Work {
        source,
        route,
        kind,
    } = work;
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    if !selected_client(context, route) {
        bail!("captured TUI is no longer the uniquely focused client");
    }
    let call = |request: Call| {
        if stopping.load(Ordering::Acquire) {
            bail!("Micro bridge is stopping");
        }
        if !selected_client(context, route) {
            bail!("captured TUI is no longer the uniquely focused client");
        }
        transport(request)
    };
    match kind {
        WorkKind::FocusSlot { pane_id } => {
            call(Call::Navigate {
                pane_id: pane_id.clone(),
            })?;
            log(format!("{source}: focused {}/{pane_id}", route.client_id));
        }
        WorkKind::Binding { binding, target } => {
            if let Binding::Action(Action::Input { text, keys }) = binding.as_ref() {
                return ordinary_input(&call, text, keys);
            }
            if target.is_none() && matches!(binding.as_ref(), Binding::Action(Action::Submit)) {
                return submit(&call);
            }
            let Some(current) = target else {
                return Ok(());
            };
            let Some(action) = binding.resolve(current.agent.as_deref().unwrap_or("")) else {
                return Ok(());
            };
            execute_action(&action, current, &call, route)
                .with_context(|| format!("{source}: {}", action_name(&action)))?;
            log(format!(
                "{source}: {} in {} for {} in {}",
                action_name(&action),
                route.client_id,
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
    stopping: Arc<AtomicBool>,
    context: Arc<Mutex<Arc<InputContext>>>,
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
        if let Err(error) = execute_work(work, &stopping, &context) {
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
    route: Option<ClientRoute>,
    target: Option<Agent>,
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
        Some(route) => {
            queue_work(
                sender,
                Work {
                    source,
                    route,
                    kind: WorkKind::Binding {
                        binding: Box::new(binding),
                        target: target.map(Box::new),
                    },
                },
            );
        }
        None => log(format!("{source} ignored: no ready Herdr input target")),
    }
}

fn handle_fired(sender: &SyncSender<Work>, fired: Vec<Fired>) {
    for fired in fired {
        let (route, target) = fired
            .context
            .map(|context| (Some(context.route), context.target))
            .unwrap_or_default();
        queue_binding(
            sender,
            Binding::Action(fired.action),
            fired.source,
            route,
            target,
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
                    context.route.clone(),
                    context.target.clone(),
                );
            }
        }
        DeviceEvent::Key { key, action } => {
            if let Some(index) = agent_slot(&key) {
                if action == 1 {
                    let slot = context.slots[index].as_ref();
                    match slot {
                        Some((route, agent)) => {
                            queue_work(
                                worker,
                                Work {
                                    source: key,
                                    route: route.clone(),
                                    kind: WorkKind::FocusSlot {
                                        pane_id: agent.pane_id.clone(),
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
            let captured = context.route.clone();
            let gesture_context = captured.as_ref().map(|route| GestureContext {
                route: route.clone(),
                target: context.target.clone(),
            });
            if matches!(key.as_str(), "ENC_CC" | "ENC_CW") {
                if let Some(binding) = binding {
                    queue_binding(worker, binding, key, captured, context.target.clone());
                }
            } else if matches!(action, 0 | 1) {
                match binding.as_ref() {
                    Some(Binding::Gesture(_)) => handle_fired(
                        worker,
                        state.gestures.handle(
                            key,
                            binding.as_ref(),
                            action == 1,
                            gesture_context,
                            Instant::now(),
                        ),
                    ),
                    Some(binding) if action == 1 => queue_binding(
                        worker,
                        binding.clone(),
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
) {
    let next = Arc::clone(&shared.lock().unwrap_or_else(|error| error.into_inner()));
    if next.controls != context.controls {
        state.gestures.clear();
    }
    *context = next;
}

pub(super) fn input_worker(
    receiver: Receiver<DeviceEvent>,
    shared: Arc<Mutex<Arc<InputContext>>>,
    work: SyncSender<Work>,
    notices: Sender<RuntimeEvent>,
    stopping: Arc<AtomicBool>,
) {
    let mut state = InputState::default();
    let mut context = Arc::clone(&shared.lock().unwrap_or_else(|error| error.into_inner()));
    while !stopping.load(Ordering::Acquire) {
        refresh_input_context(&shared, &mut context, &mut state);
        if stopping.load(Ordering::Acquire) {
            break;
        }
        handle_fired(&work, state.gestures.drain_due(Instant::now()));
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
            refresh_input_context(&shared, &mut context, &mut state);
            handle_device_event(event, &mut state, &context, &work, &notices);
        }
    }
    state.gestures.clear();
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, fs, os::unix::fs::PermissionsExt, sync::mpsc};

    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn agent(pane: &str, kind: &str) -> Agent {
        serde_json::from_value(json!({
            "agent": kind,
            "agent_status": "idle",
            "workspace_id": "w1",
            "tab_id": "w1:t1",
            "pane_id": pane,
            "focused": true,
            "state_change_seq": 1,
            "state_labels": [],
            "tokens": []
        }))
        .unwrap()
    }

    fn route(name: &str) -> ClientRoute {
        ClientRoute {
            client_id: name.into(),
            endpoint_id: "local/work".into(),
            boot_id: Some("boot".into()),
            socket_path: format!("/tmp/{name}.sock").into(),
        }
    }

    fn selecting(route: Option<ClientRoute>) -> Mutex<Arc<InputContext>> {
        let mut context = input_context();
        context.route = route;
        Mutex::new(Arc::new(context))
    }

    fn prompt_call(text: &str) -> Call {
        Call::Call {
            method: "agent.prompt".into(),
            params: json!({"target":"pane","text":text}),
        }
    }

    #[test]
    fn script_context_drives_the_bundled_adapter() {
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path();
        let fake_herdr = dir.join("herdr");
        let calls = dir.join("calls");
        let environment = dir.join("environment");
        fs::write(
            &fake_herdr,
            r#"#!/bin/sh
if [ ! -e "$HERDR_TEST_ENV" ]; then
    printf '%s\n' "$HERDR_FRONTEND_SOCKET" "$HERDR_PANE_ID" "$PWD" > "$HERDR_TEST_ENV"
fi
printf '%s\n' --call "$@" >> "$HERDR_TEST_LOG"
"#,
        )
        .unwrap();
        fs::set_permissions(&fake_herdr, fs::Permissions::from_mode(0o700)).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = Config::default();
        let target_route = route("work");

        let Action::Script { command, args } = key_binding(&config.controls, "ENC_CC", 1)
            .unwrap()
            .resolve("codex")
            .unwrap()
        else {
            panic!("expected Codex script action");
        };
        let mut child = script_command(&command, &args, root, &target_route, "w9:p4").unwrap();
        child
            .env("HERDR_MICRO_BIN_PATH", &fake_herdr)
            .env("HERDR_TEST_LOG", &calls)
            .env("HERDR_TEST_ENV", &environment);
        run_command_with_timeout(&mut child, COMMAND_TIMEOUT).unwrap();
        assert_eq!(
            fs::read_to_string(&environment).unwrap(),
            format!(
                "{}\nw9:p4\n{}\n",
                target_route.socket_path.display(),
                root.display()
            )
        );
        assert_eq!(
            fs::read_to_string(&calls).unwrap(),
            "--call\nclient\ninput\nkeys\nalt+.\n"
        );

        fs::remove_file(&calls).unwrap();
        let Action::Script { command, args } = key_binding(&config.controls, "ENC_CW", 1)
            .unwrap()
            .resolve("claude")
            .unwrap()
        else {
            panic!("expected Claude script action");
        };
        let mut child = script_command(&command, &args, root, &target_route, "w9:p4").unwrap();
        child
            .env("HERDR_MICRO_BIN_PATH", &fake_herdr)
            .env("HERDR_TEST_LOG", &calls)
            .env("HERDR_TEST_ENV", &environment);
        run_command_with_timeout(&mut child, COMMAND_TIMEOUT).unwrap();
        assert_eq!(
            fs::read_to_string(&calls).unwrap(),
            "--call\nclient\ninput\ntext\n/effort\n--call\nclient\ninput\nkeys\nenter\n--call\nclient\ninput\nkeys\nleft\n--call\nclient\ninput\nkeys\nenter\n"
        );
    }

    #[test]
    fn oai_agent_keys_map_to_slots() {
        assert_eq!(agent_slot("AG00"), Some(0));
        assert_eq!(agent_slot("AG05"), Some(5));
        assert_eq!(agent_slot("AG06"), None);
    }
    fn input_context() -> InputContext {
        InputContext {
            controls: Config::default().controls,
            route: Some(route("work")),
            target: Some(agent("pane", "codex")),
            slots: vec![Some((route("other-endpoint"), agent("slot-pane", "pi")))],
        }
    }

    #[test]
    fn queued_bindings_use_captured_agent_context_without_reading_inventory() {
        let context = input_context();
        let actions = [
            Action::Prompt {
                prompt: "hello".into(),
                submit: Some(true),
            },
            Action::Submit,
            Action::Fast,
            Action::FocusPane {
                direction: Direction::Left,
            },
        ];
        let (sender, receiver) = mpsc::sync_channel(8);
        for action in actions {
            queue_binding(
                &sender,
                Binding::ByAgent([("codex".into(), Some(action))].into()),
                "test".into(),
                context.route.clone(),
                context.target.clone(),
            );
        }
        let calls = RefCell::new(Vec::new());
        let caller = |call: Call| {
            calls.borrow_mut().push(call);
            Ok(())
        };
        let selected = selecting(context.route.clone());
        for work in receiver.try_iter() {
            assert_eq!(work.route.client_id, "work");
            run_work(&work, &AtomicBool::new(false), &selected, &caller).unwrap();
        }
        assert_eq!(
            calls.into_inner(),
            [
                prompt_call("hello"),
                Call::Input(Input::Keys(vec!["enter".into()])),
                prompt_call("/fast"),
                Call::Call {
                    method: "pane.focus_direction".into(),
                    params: json!({"pane_id":"pane","direction":"left"}),
                },
            ]
        );
    }

    #[test]
    fn stopped_worker_sends_nothing_for_bindings_slots_or_scripts() {
        let panic_call = |_: Call| -> Result<()> { panic!("stopped worker sent RPC") };
        for kind in [
            WorkKind::Binding {
                binding: Box::new(Binding::Action(Action::Submit)),
                target: input_context().target.map(Box::new),
            },
            WorkKind::Binding {
                binding: Box::new(Binding::Action(Action::Script {
                    command: "this-must-not-run".into(),
                    args: vec![],
                })),
                target: input_context().target.map(Box::new),
            },
            WorkKind::FocusSlot {
                pane_id: "pane".into(),
            },
        ] {
            run_work(
                &Work {
                    source: "test".into(),
                    route: route("work"),
                    kind,
                },
                &AtomicBool::new(true),
                &selecting(Some(route("work"))),
                &panic_call,
            )
            .unwrap();
        }
    }

    #[test]
    fn pending_gestures_keep_initial_destination_across_focus_and_inventory_changes() {
        let initial = Arc::new(input_context());
        let shared = Mutex::new(Arc::clone(&initial));
        let mut context = Arc::clone(&initial);
        let mut state = InputState::default();
        let now = Instant::now();
        let binding = Binding::Gesture(Box::new(crate::config::GestureBinding {
            hold: Some(Action::Fast),
            hold_ms: Some(10),
            ..Default::default()
        }));
        state.gestures.handle(
            "key",
            Some(&binding),
            true,
            Some(GestureContext {
                route: initial.route.clone().unwrap(),
                target: initial.target.clone(),
            }),
            now,
        );
        for change in ["client", "agent", "inventory", "unavailable"] {
            let mut next = input_context();
            match change {
                "client" => next.route = Some(route("personal")),
                "agent" => next.target = Some(agent("pane", "claude")),
                "inventory" => next.slots.clear(),
                "unavailable" => {
                    next.route = None;
                    next.target = None;
                }
                _ => unreachable!(),
            }
            *shared.lock().unwrap() = Arc::new(next);
            refresh_input_context(&shared, &mut context, &mut state);
            assert!(state.gestures.next_deadline().is_some(), "{change}");
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        handle_fired(
            &sender,
            state.gestures.drain_due(now + Duration::from_millis(20)),
        );
        let work = receiver.try_recv().unwrap();
        assert_eq!(work.route, initial.route.clone().unwrap());
        let calls = RefCell::new(Vec::new());
        let caller = |call: Call| {
            calls.borrow_mut().push(call);
            Ok(())
        };
        run_work(
            &work,
            &AtomicBool::new(false),
            &selecting(initial.route.clone()),
            &caller,
        )
        .unwrap();
        assert_eq!(calls.into_inner(), [prompt_call("/fast")]);
    }

    #[test]
    fn device_events_capture_binding_and_slot_destinations() {
        let context = input_context();
        let (sender, receiver) = mpsc::sync_channel(2);
        let (notices, _) = mpsc::channel();
        let mut state = InputState::default();
        for key in ["ACT09", "AG00"] {
            handle_device_event(
                DeviceEvent::Key {
                    key: key.into(),
                    action: 1,
                },
                &mut state,
                &context,
                &sender,
                &notices,
            );
        }
        let binding = receiver.try_recv().unwrap();
        assert_eq!(binding.route, context.route.unwrap());
        assert!(
            matches!(binding.kind, WorkKind::Binding {target, ..} if target.as_deref() == context.target.as_ref())
        );
        let slot = receiver.try_recv().unwrap();
        assert_eq!(slot.route.client_id, "other-endpoint");
        assert!(matches!(slot.kind, WorkKind::FocusSlot {pane_id} if pane_id == "slot-pane"));
    }
    #[test]
    fn lost_conflicting_or_changed_focus_rejects_captured_work_without_dispatch() {
        let work = Work {
            source: "held".into(),
            route: route("work"),
            kind: WorkKind::Binding {
                binding: Box::new(Binding::Action(Action::Input {
                    text: Some("hello".into()),
                    keys: None,
                })),
                target: None,
            },
        };
        let panic_call = |_: Call| -> Result<()> { panic!("unfocused work dispatched") };
        for selected in [None, Some(route("another"))] {
            assert!(
                run_work(
                    &work,
                    &AtomicBool::new(false),
                    &selecting(selected),
                    &panic_call
                )
                .is_err()
            );
        }
    }
}
