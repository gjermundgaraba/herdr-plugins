//! Input dispatch and action execution: device events become jumps and
//! queued work, and the action worker types into the frontmost app while the
//! client captured at the press is still the followed one and has focus.

use anyhow::{Context, Result, anyhow};
use codex_micro::DeviceEvent;
use herdr_client::{
    ClientJumped,
    follow::{ClientKey, JumpTarget},
};
use herdr_device::{
    Focus, InputError, JumpDispatcher, JumpError, Keystroke, OsInput, PostError, Refusal,
    SUBMIT_SETTLE, type_inputs,
};
use std::{
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    config::{Action, Binding, Direction, Modifier, key_action_code, key_binding},
    gestures::{Fired, GestureContext, GestureDispatcher},
    macos,
    protocol::{SLOT_COUNT, joystick_event},
};

use super::{RuntimeEvent, log, reconcile::InputContext};

#[derive(Default)]
struct InputState {
    gestures: GestureDispatcher,
    last_joystick_sector: Option<u8>,
}

/// A prompt or submit binding, with the client followed at the press.
pub(super) struct Work {
    source: String,
    client: ClientKey,
    /// Kind of the agent that client had focused, for `byAgent` bindings.
    agent: Option<String>,
    binding: Box<Binding>,
}

fn action_name(action: &Action) -> &'static str {
    match action {
        Action::Prompt { .. } => "prompt",
        Action::Submit => "submit",
        Action::Key { .. } => "key",
    }
}

fn execute_work(work: Work, focus: &Focus) -> Result<()> {
    run_work(&work, focus, &herdr_device::macos::post, &|| {
        thread::sleep(SUBMIT_SETTLE)
    })
}

/// Execute captured work. `focus` is checked before every keystroke: the
/// bridge must still be running, and the captured client still followed with
/// its window focused in the last published report. Otherwise the rest is
/// dropped.
fn run_work(
    work: &Work,
    focus: &Focus,
    post: &dyn Fn(Keystroke<'_>) -> Result<(), PostError>,
    settle: &dyn Fn(),
) -> Result<()> {
    let Work {
        source,
        client,
        agent,
        binding,
    } = work;
    let Some(action) = binding.resolve(agent.as_deref().unwrap_or("")) else {
        return Ok(());
    };
    let inputs = match &action {
        Action::Prompt { prompt, submit } => OsInput::prompt(prompt, submit.unwrap_or(true)),
        Action::Submit => vec![OsInput::Enter],
        // Key actions cannot reach the worker: top-level keys execute inline in
        // queue_binding and byAgent keys are rejected at parse.
        Action::Key { .. } => return Err(anyhow!("key action routed to the action worker")),
    };
    match type_inputs(&inputs, client, focus, post, settle) {
        // The bridge is going away; there is nobody to tell.
        Err(InputError::Refused(Refusal::Stopping)) => return Ok(()),
        result => result.with_context(|| format!("{source}: {}", action_name(&action)))?,
    }
    log(format!(
        "{source}: {} in {} client {} for {}",
        action_name(&action),
        client.socket_path.display(),
        client.client_id,
        agent.as_deref().unwrap_or("no agent"),
    ));
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

/// Runs queued work in order. Once `focus` is stopped, what is still queued
/// is dropped.
pub(super) fn action_worker(receiver: Receiver<Work>, focus: Focus) {
    while !focus.is_stopping() {
        let work = match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(work) => work,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if focus.is_stopping() {
            break;
        }
        if let Err(error) = execute_work(work, &focus) {
            log(format!("control failed: {error:#}"));
        }
    }
}

/// The log line for a finished Agent-key jump. There is none once the
/// bridge stops, or when a later press replaced the jump: that later jump
/// reports for itself.
fn jump_line(source: &str, result: Result<ClientJumped, JumpError>) -> Option<String> {
    Some(match result {
        Err(error) if error.is_superseded() => return None,
        Ok(jumped) => format!(
            "{source}: jumped to {}/{}",
            jumped.endpoint_id, jumped.pane_id
        ),
        Err(JumpError::Refused(Refusal::Stopping)) => return None,
        Err(JumpError::Refused(refusal)) => format!("{source} ignored: {refusal}"),
        Err(error) => format!("{source}: {error}"),
    })
}

fn execute_key(key: Option<&str>, keycode: Option<u16>, modifiers: &[Modifier]) -> Result<()> {
    let code = key_action_code(key, keycode).map_err(|error| anyhow!(error))?;
    Ok(macos::post_key(code, modifiers)?)
}

fn queue_binding(
    sender: &SyncSender<Work>,
    binding: Binding,
    source: String,
    client: Option<ClientKey>,
    agent: Option<String>,
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
    match client {
        Some(client) => {
            queue_work(
                sender,
                Work {
                    source,
                    client,
                    agent,
                    binding: Box::new(binding),
                },
            );
        }
        None => log(format!("{source} ignored: no followed Herdr client")),
    }
}

fn handle_fired(sender: &SyncSender<Work>, fired: Vec<Fired>) {
    for fired in fired {
        let (client, agent) = fired
            .context
            .map(|context| (Some(context.client), context.agent))
            .unwrap_or_default();
        queue_binding(
            sender,
            Binding::Action(fired.action),
            fired.source,
            client,
            agent,
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
    jump: &mut dyn FnMut(String, JumpTarget),
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
                    context.client.clone(),
                    context.agent.clone(),
                );
            }
        }
        DeviceEvent::Key { key, action } => {
            if let Some(index) = agent_slot(&key) {
                if action == 1 {
                    match context.slots.get(index).and_then(Option::as_ref) {
                        Some(target) => jump(key, target.clone()),
                        None => log(format!("{key} ignored: no ready Agent slot")),
                    }
                }
                return;
            }
            let binding = key_binding(controls, &key, action);
            let captured = context.client.clone();
            let gesture_context = captured.as_ref().map(|client| GestureContext {
                client: client.clone(),
                agent: context.agent.clone(),
            });
            if matches!(key.as_str(), "ENC_CC" | "ENC_CW") {
                if let Some(binding) = binding {
                    queue_binding(worker, binding, key, captured, context.agent.clone());
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
                        context.agent.clone(),
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
    focus: Focus,
) {
    let mut state = InputState::default();
    let mut context = Arc::clone(&shared.lock().unwrap_or_else(|error| error.into_inner()));
    // Agent-key jumps stay off the action queue: a daemon holds a jump
    // until its client answers. This loop receives the presses in order,
    // which is the order the dispatcher sends them in.
    let mut jumps = JumpDispatcher::new(focus.clone());
    let mut jump = |source: String, target: JumpTarget| {
        jumps.press(target, move |result| {
            if let Some(line) = jump_line(&source, result) {
                log(line);
            }
        });
    };
    while !focus.is_stopping() {
        refresh_input_context(&shared, &mut context, &mut state);
        if focus.is_stopping() {
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
            if focus.is_stopping() {
                break;
            }
            refresh_input_context(&shared, &mut context, &mut state);
            handle_device_event(event, &mut state, &context, &work, &notices, &mut jump);
        }
    }
    state.gestures.clear();
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, sync::mpsc};

    use super::*;
    use crate::config::Config;
    use herdr_client::ClientJumpError;

    fn client(session: &str) -> ClientKey {
        ClientKey {
            socket_path: format!("/run/herdr/{session}/herdr.sock").into(),
            boot_id: "daemon-boot".into(),
            client_id: 7,
        }
    }

    fn target(session: &str) -> JumpTarget {
        JumpTarget {
            client: client(session),
            endpoint_id: "ssh:build".into(),
            boot_id: "boot-b".into(),
            pane_id: "w1:p4".into(),
        }
    }

    fn input_context() -> InputContext {
        InputContext {
            controls: Config::default().controls,
            client: Some(client("work")),
            agent: Some("codex".into()),
            slots: vec![Some(target("work"))],
        }
    }

    /// What the dispatch threads check: `client` is followed and its
    /// window has focus.
    fn focused_on(client: Option<ClientKey>) -> Focus {
        let focus = Focus::new(Arc::default());
        focus.set(client, true);
        focus
    }

    fn work(binding: Binding, agent: Option<&str>) -> Work {
        Work {
            source: "test".into(),
            client: client("work"),
            agent: agent.map(str::to_owned),
            binding: Box::new(binding),
        }
    }

    fn prompt(text: &str, submit: Option<bool>) -> Action {
        Action::Prompt {
            prompt: text.into(),
            submit,
        }
    }

    /// Runs `work` against `focus` and returns the result and what a
    /// recording sink received. Tests never post a real macOS event.
    fn run(work: &Work, focus: &Focus) -> (Result<()>, Vec<OsInput>) {
        let events = RefCell::new(Vec::new());
        let post = |keystroke: Keystroke<'_>| {
            events.borrow_mut().push(match keystroke {
                Keystroke::Text(units) => OsInput::Text(String::from_utf16_lossy(units)),
                Keystroke::Enter => OsInput::Enter,
            });
            Ok(())
        };
        let result = run_work(work, focus, &post, &|| ());
        (result, events.into_inner())
    }

    #[test]
    fn oai_agent_keys_map_to_slots() {
        assert_eq!(agent_slot("AG00"), Some(0));
        assert_eq!(agent_slot("AG05"), Some(5));
        assert_eq!(agent_slot("AG06"), None);
    }

    #[test]
    fn prompt_and_submit_become_keyboard_events_for_the_followed_client() {
        let focused = focused_on(Some(client("work")));
        for (binding, agent, expected) in [
            (
                Binding::Action(prompt("/model", None)),
                Some("codex"),
                vec![OsInput::Text("/model".into()), OsInput::Enter],
            ),
            (
                Binding::Action(prompt("draft", Some(false))),
                None,
                vec![OsInput::Text("draft".into())],
            ),
            (Binding::Action(Action::Submit), None, vec![OsInput::Enter]),
            // byAgent follows the kind of agent the client had focused.
            (
                Binding::ByAgent(
                    [
                        ("codex".to_owned(), Some(prompt("/fast", Some(true)))),
                        ("default".to_owned(), None),
                    ]
                    .into(),
                ),
                Some("codex"),
                vec![OsInput::Text("/fast".into()), OsInput::Enter],
            ),
            (
                Binding::ByAgent(
                    [
                        ("codex".to_owned(), Some(prompt("/fast", Some(true)))),
                        ("default".to_owned(), None),
                    ]
                    .into(),
                ),
                Some("claude"),
                vec![],
            ),
            // Without a focused agent only the default variant applies.
            (
                Binding::ByAgent(
                    [
                        ("codex".to_owned(), Some(prompt("/fast", None))),
                        ("default".to_owned(), Some(Action::Submit)),
                    ]
                    .into(),
                ),
                None,
                vec![OsInput::Enter],
            ),
        ] {
            let (result, events) = run(&work(binding, agent), &focused);
            result.unwrap();
            assert_eq!(events, expected);
        }
    }

    #[test]
    fn prompt_and_submit_are_suppressed_without_a_followed_and_focused_client() {
        // At the press: no followed client means nothing is queued.
        let (sender, receiver) = mpsc::sync_channel(4);
        let (notices, _notices) = mpsc::channel();
        let mut unfocused = input_context();
        unfocused.client = None;
        unfocused.slots = vec![None; SLOT_COUNT];
        let mut state = InputState::default();
        for key in ["ACT09", "ACT12", "ENC_CLK", "AG00"] {
            handle_device_event(
                DeviceEvent::Key {
                    key: key.into(),
                    action: 1,
                },
                &mut state,
                &unfocused,
                &sender,
                &notices,
                &mut |_, _| panic!("a jump was started without a followed client"),
            );
        }
        assert!(receiver.try_recv().is_err());

        // At dispatch: another client is followed by now, or the captured
        // one is behind another app. Nothing is typed.
        let behind = Focus::new(Arc::default());
        behind.set(Some(client("work")), false);
        for (focus, refusal) in [
            (focused_on(Some(client("default"))), Refusal::NotFollowed),
            (behind, Refusal::NotFocused),
        ] {
            let (result, events) = run(
                &work(Binding::Action(prompt("/copy", None)), Some("codex")),
                &focus,
            );
            let error = format!("{:#}", result.unwrap_err());
            assert!(error.contains(&refusal.to_string()), "{error}");
            assert!(events.is_empty());
        }
    }

    #[test]
    fn a_prompt_stops_at_the_chunk_after_the_follower_saw_focus_lost() {
        use crate::daemon::{
            clients_changed,
            reconcile::tests::{client as reporting, daemon},
        };
        use herdr_client::follow::Directory;
        let listing = |focused| vec![daemon("work", vec![reporting(7, focused)])];
        let front = Directory::default().updated(listing(Some(true)));
        let behind = front.updated(listing(Some(false)));
        let key = front.followed_client().unwrap().key();
        // The loop published the client with its window in front.
        let focus = Focus::new(Arc::default());
        focus.follow(front.followed_client());
        let (events, loop_events) = mpsc::channel();

        let text = "a prompt that takes two keyboard events";
        let typed = RefCell::new(Vec::new());
        let post = |keystroke: Keystroke<'_>| {
            if let Keystroke::Text(units) = keystroke {
                typed.borrow_mut().push(String::from_utf16_lossy(units));
            }
            // Another app takes focus while the first chunk is posted. The
            // follower says so on its own thread; the loop takes no turn.
            assert!(clients_changed(&focus, &events, behind.clone()));
            Ok(())
        };
        let work = Work {
            source: "test".into(),
            client: key,
            agent: None,
            binding: Box::new(Binding::Action(prompt(text, None))),
        };
        let error = format!("{:#}", run_work(&work, &focus, &post, &|| ()).unwrap_err());
        assert!(error.contains(&Refusal::NotFocused.to_string()), "{error}");
        assert_eq!(*typed.borrow(), [&text[..20]]);
        // The event that carries the loss is still waiting for the loop.
        assert!(matches!(
            loop_events.try_recv(),
            Ok(RuntimeEvent::Clients(_))
        ));
    }

    #[test]
    fn a_stopping_bridge_sends_nothing_and_reports_nothing() {
        for binding in [
            Binding::Action(Action::Submit),
            Binding::Action(prompt("/copy", None)),
        ] {
            let focus = focused_on(Some(client("work")));
            focus.stop();
            let (result, events) = run(&work(binding, Some("codex")), &focus);
            result.unwrap();
            assert!(events.is_empty());
        }
    }

    #[test]
    fn pending_gestures_keep_the_client_and_agent_of_the_first_press() {
        let initial = Arc::new(input_context());
        let shared = Mutex::new(Arc::clone(&initial));
        let mut context = Arc::clone(&initial);
        let mut state = InputState::default();
        let now = Instant::now();
        let binding = Binding::Gesture(Box::new(crate::config::GestureBinding {
            hold: Some(prompt("/held", None)),
            hold_ms: Some(50),
            ..Default::default()
        }));
        state.gestures.handle(
            "key",
            Some(&binding),
            true,
            Some(GestureContext {
                client: initial.client.clone().unwrap(),
                agent: initial.agent.clone(),
            }),
            now,
        );
        for change in ["client", "agent", "inventory", "unavailable"] {
            let mut next = input_context();
            match change {
                "client" => next.client = Some(client("personal")),
                "agent" => next.agent = Some("claude".into()),
                "inventory" => next.slots.clear(),
                "unavailable" => {
                    next.client = None;
                    next.agent = None;
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
            state.gestures.drain_due(now + Duration::from_millis(60)),
        );
        let work = receiver.try_recv().unwrap();
        assert_eq!(Some(&work.client), initial.client.as_ref());
        assert_eq!(work.agent.as_deref(), Some("codex"));

        // It is typed only if that same client has focus again by then.
        let (result, events) = run(&work, &focused_on(None));
        assert!(result.is_err() && events.is_empty());
        let (result, events) = run(&work, &focused_on(initial.client.clone()));
        result.unwrap();
        assert_eq!(events, [OsInput::Text("/held".into()), OsInput::Enter]);
    }

    #[test]
    fn device_events_capture_binding_and_slot_destinations() {
        let context = input_context();
        let (sender, receiver) = mpsc::sync_channel(2);
        let (notices, _notices) = mpsc::channel();
        let mut state = InputState::default();
        let jumps = RefCell::new(Vec::new());
        // A press and its release: only the press acts.
        for (key, action) in [
            ("ACT09", 1),
            ("ACT09", 0),
            ("AG00", 1),
            ("AG00", 0),
            ("AG03", 1),
        ] {
            handle_device_event(
                DeviceEvent::Key {
                    key: key.into(),
                    action,
                },
                &mut state,
                &context,
                &sender,
                &notices,
                &mut |source, target| jumps.borrow_mut().push((source, target)),
            );
        }
        let binding = receiver.try_recv().unwrap();
        assert!(receiver.try_recv().is_err());
        assert_eq!(Some(&binding.client), context.client.as_ref());
        assert_eq!(binding.agent, context.agent);
        assert_eq!(
            *binding.binding,
            Binding::Action(prompt("/copy", Some(true)))
        );
        // The slot key starts exactly one jump, to the slot's captured pane;
        // an empty slot starts none.
        assert_eq!(jumps.into_inner(), [("AG00".to_owned(), target("work"))]);
    }

    #[test]
    fn each_jump_outcome_becomes_one_log_line() {
        let failed = |source| JumpError::Failed {
            endpoint_id: "ssh:build".into(),
            pane_id: "w1:p4".into(),
            source,
        };
        for (result, expected) in [
            (
                Ok(ClientJumped {
                    client_id: 7,
                    endpoint_id: "ssh:build".into(),
                    pane_id: "w1:p4".into(),
                }),
                Some("AG02: jumped to ssh:build/w1:p4".to_owned()),
            ),
            (
                Err(failed(ClientJumpError::EndpointUnavailable)),
                Some("AG02: jump to ssh:build/w1:p4: the client could not reach the endpoint; the jump may still have happened".into()),
            ),
            (
                Err(Refusal::NotFollowed.into()),
                Some(format!("AG02 ignored: {}", Refusal::NotFollowed)),
            ),
            // A later press replaced this jump: expected, and not logged.
            (Err(failed(ClientJumpError::Superseded)), None),
            (Err(JumpError::Replaced), None),
            (Err(JumpError::Overtaken), None),
            (Err(Refusal::Stopping.into()), None),
        ] {
            assert_eq!(jump_line("AG02", result), expected);
        }
    }
}
