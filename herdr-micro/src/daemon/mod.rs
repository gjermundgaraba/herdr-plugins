//! The long-lived Codex Micro bridge.  Keep the policy here; HID framing,
//! Herdr client following, gestures, and macOS operations live in their
//! small modules.
//! The event loop and logging live in this module, input dispatch in
//! `dispatch`, and Herdr/device state reconciliation in `reconcile`.

mod device;
mod dispatch;
mod reconcile;

use anyhow::{Result, anyhow};
use herdr_client::open_rotating_log;
use signal_hook::consts::{SIGINT, SIGTERM};
use std::io::Write;
use std::{
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, TryRecvError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    config::{config_path, enabled_buttons, load, provision},
    control::listen_for_control,
};
use dispatch::{action_worker, input_worker};
use herdr_client::{
    follow::{self, Directory},
    sessions,
};
use herdr_device::Focus;
use reconcile::{
    InputContext, State, apply_clients_update, apply_config_load, apply_service_status,
    device_output, handle_input_disconnect, publish, reconcile_active, refresh_owner,
    update_input_context,
};

const CONFIG_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const OWNER_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
const WORK_QUEUE_CAPACITY: usize = 16;
// A running policy client is replaced when this changes: with the lighting IPC
// contract, and with the Herdr API the bridge follows (13: daemon client API).
pub const DAEMON_PROTOCOL_VERSION: u32 = 13;

static LOG_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

enum RuntimeEvent {
    Clients(Box<Directory>),
    InputDisconnected(String),
    Device(device::Update),
}

fn apply_runtime_event(event: RuntimeEvent, state: &mut State, stopping: &AtomicBool) {
    match event {
        RuntimeEvent::Clients(directory) => apply_clients_update(state, *directory, stopping),
        RuntimeEvent::InputDisconnected(error) => {
            handle_input_disconnect(state, error);
            reconcile_active(state, stopping);
        }
        RuntimeEvent::Device(device::Update::Status(status)) => {
            if apply_service_status(state, &status) {
                if let Some(error) = &status.last_error {
                    log(format!("device unavailable: {error}"));
                } else if let Some(info) = &status.device {
                    log(format!(
                        "device connected: transport={:?}, firmware={}",
                        info.transport, info.firmware
                    ));
                }
            }
        }
        RuntimeEvent::Device(device::Update::Error(error)) => {
            log_changed(
                &mut state.last_device_error,
                error,
                "device update failed: ",
            );
        }
    }
}

fn apply_runtime_batch(
    events: impl IntoIterator<Item = RuntimeEvent>,
    state: &mut State,
    stopping: &AtomicBool,
    dirty: &mut bool,
) {
    *dirty = true;
    // Apply transitions in order and bound the batch to keep owner/config checks responsive.
    for event in events.into_iter().take(256) {
        if stopping.load(Ordering::Acquire) {
            break;
        }
        apply_runtime_event(event, state, stopping);
    }
}

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

pub(super) fn log_changed(last: &mut String, next: String, prefix: &str) {
    if *last == next {
        return;
    }
    log(format!("{prefix}{next}"));
    *last = next;
}

fn format_timestamp(time: SystemTime) -> String {
    let elapsed = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = elapsed.as_secs();
    let (year, month, day) = civil_from_days(seconds / 86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3_600 % 24,
        seconds / 60 % 60,
        seconds % 60,
        elapsed.subsec_millis(),
    )
}

/// Proleptic Gregorian date for a day count since 1970-01-01
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_point = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_point + 2) / 5 + 1;
    let month = if month_point < 10 {
        month_point + 3
    } else {
        month_point - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// Publish derived state once after a change, preserving the idle owner poll.
fn reconcile_dirty(
    dirty: &mut bool,
    state: &mut State,
    stopping: &AtomicBool,
    input_context: &Mutex<Arc<InputContext>>,
    focus: &Focus,
    status: &Arc<Mutex<serde_json::Value>>,
) -> Option<device::Output> {
    if !std::mem::take(dirty) {
        return None;
    }
    reconcile_active(state, stopping);
    update_input_context(input_context, focus, state);
    publish(status, state);
    Some(device_output(state))
}

/// Ends routing for shutdown. Queued and half-typed work is dropped, and the
/// last published client is forgotten: the follower stops publishing now.
fn stop_routing(state: &mut State, focus: &Focus, input_context: &Mutex<Arc<InputContext>>) {
    state.disable_routing();
    focus.stop();
    update_input_context(input_context, focus, state);
}

/// The follower's callback, run on the follower's thread. The loop applies
/// the directory on its next turn, and a prompt being typed must not go on
/// into the app that took focus meanwhile: what the followed client lost is
/// in force from here on. What it gained waits for the loop, which also
/// knows whether the bridge routes at all.
fn clients_changed(
    focus: &Focus,
    events: &mpsc::Sender<RuntimeEvent>,
    directory: Directory,
) -> bool {
    // Before the event, so the loss is in force by the time anything can
    // act on it.
    focus.limit(directory.followed_client());
    events
        .send(RuntimeEvent::Clients(Box::new(directory)))
        .is_ok()
}

/// Run the bridge in the foreground.  `main`/the start action owns process
/// detachment; this function deliberately owns only the live daemon.
pub fn run_daemon() -> Result<()> {
    let config_path = config_path().map_err(|error| anyhow!(error))?;
    provision(&config_path).map_err(|error| anyhow!(error))?;
    let config = load(&config_path).map_err(|error| anyhow!(error))?;
    let startup_enabled_buttons = enabled_buttons(&config.controls);
    let (device_tx, device_rx) = mpsc::channel();
    let (runtime_tx, runtime_rx) = mpsc::channel();
    let (work_tx, work_rx) = mpsc::sync_channel(WORK_QUEUE_CAPACITY);
    let stopping = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, Arc::clone(&stopping))?;
    signal_hook::flag::register(SIGTERM, Arc::clone(&stopping))?;
    let focus = Focus::new(Arc::clone(&stopping));
    // Every local session is its own daemon; the followed client may be
    // attached to any of them, so all are followed.
    follow::spawn(
        sessions::discover,
        {
            let focus = focus.clone();
            let runtime_tx = runtime_tx.clone();
            move |directory| clients_changed(&focus, &runtime_tx, directory)
        },
        log,
        stopping.clone(),
    );
    let device = device::Worker::spawn(
        device_tx.clone(),
        {
            let runtime_tx = runtime_tx.clone();
            move |update| {
                let _ = runtime_tx.send(RuntimeEvent::Device(update));
            }
        },
        Arc::clone(&stopping),
    )?;
    let mut state = State::new(config);
    let input_context = Arc::new(Mutex::new(Arc::new(InputContext::new(&state.config))));
    let worker = thread::spawn({
        let focus = focus.clone();
        move || action_worker(work_rx, focus)
    });
    let input = thread::spawn({
        let context = Arc::clone(&input_context);
        let work = work_tx.clone();
        let focus = focus.clone();
        move || input_worker(device_rx, context, work, runtime_tx, focus)
    });
    let status = Arc::new(Mutex::new(state.status()));
    let server = listen_for_control(Arc::clone(&status), Arc::clone(&stopping))?;
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let (control_result_tx, control_result_rx) = mpsc::sync_channel(1);
    let control_thread = thread::spawn(move || {
        let _ = control_result_tx.send(server.run_with_shutdown(&shutdown_rx));
    });
    let mut control_error = None;
    let mut config_due = Instant::now();
    let mut owner_due = Instant::now();
    let mut dirty = true;
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
        if stopping.load(Ordering::Acquire) {
            continue;
        }
        let now = Instant::now();
        if now >= owner_due {
            owner_due = now + OWNER_REFRESH_INTERVAL;
            dirty |= refresh_owner(&mut state);
        }
        if now >= config_due {
            config_due = now + CONFIG_REFRESH_INTERVAL;
            dirty |= apply_config_load(&mut state, load(&config_path), startup_enabled_buttons);
        }
        if let Some(output) = reconcile_dirty(
            &mut dirty,
            &mut state,
            &stopping,
            &input_context,
            &focus,
            &status,
        ) {
            device.set_output(output);
        }

        let deadline = config_due.min(owner_due);
        match runtime_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(event) => {
                apply_runtime_batch(
                    std::iter::once(event).chain(runtime_rx.try_iter()),
                    &mut state,
                    &stopping,
                    &mut dirty,
                );
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    log("stopping");
    stop_routing(&mut state, &focus, &input_context);
    device.shutdown();
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

    #[test]
    fn dirty_publication_covers_startup_errors_owner_changes_and_shutdown() {
        let config = crate::config::Config::default();
        let mut state = State::new(config.clone());
        let context = Mutex::new(Arc::new(InputContext::new(&config)));
        let focus = Focus::new(Arc::default());
        let status = Arc::new(Mutex::new(serde_json::Value::Null));
        let stopping = AtomicBool::new(false);
        let mut dirty = true;
        assert!(
            reconcile_dirty(&mut dirty, &mut state, &stopping, &context, &focus, &status).is_some()
        );
        assert_eq!(*status.lock().unwrap(), state.status());
        let initial_context = Arc::clone(&context.lock().unwrap());
        for _ in 0..20 {
            assert!(
                reconcile_dirty(&mut dirty, &mut state, &stopping, &context, &focus, &status)
                    .is_none()
            );
            assert!(Arc::ptr_eq(&initial_context, &context.lock().unwrap()));
        }

        apply_runtime_batch(
            [RuntimeEvent::Device(device::Update::Error(
                "write failed".into(),
            ))],
            &mut state,
            &stopping,
            &mut dirty,
        );
        assert!(
            reconcile_dirty(&mut dirty, &mut state, &stopping, &context, &focus, &status).is_some()
        );
        assert_eq!(status.lock().unwrap()["deviceError"], "write failed");

        for owner in [Some(codex_micro::ExternalOwner::Input), None] {
            state.owner = owner;
            dirty = true;
            assert!(
                reconcile_dirty(&mut dirty, &mut state, &stopping, &context, &focus, &status)
                    .is_some()
            );
            assert_eq!(
                status.lock().unwrap()["device"] == "yielded",
                owner.is_some()
            );
        }
        state.routing_ready = true;
        state.disable_routing();
        stopping.store(true, Ordering::Release);
        publish(&status, &state);
        assert_ne!(status.lock().unwrap()["routing"], "ready");
    }

    #[test]
    fn runtime_batches_apply_the_latest_directory_and_bound_work() {
        use reconcile::tests::{client, daemon};
        let config = crate::config::Config::default();
        let mut state = State::new(config);
        let stopping = AtomicBool::new(false);
        let event = |session: &str| {
            RuntimeEvent::Clients(Box::new(
                Directory::default().updated(vec![daemon(session, vec![client(7, Some(true))])]),
            ))
        };
        let selected = |state: &State| {
            state
                .directory
                .followed_client()
                .map(|client| client.session.to_owned())
        };
        apply_runtime_event(event("default"), &mut state, &stopping);
        let mut dirty = false;
        apply_runtime_batch(
            [event("work"), event("default")],
            &mut state,
            &stopping,
            &mut dirty,
        );
        assert!(dirty);
        assert_eq!(selected(&state).as_deref(), Some("default"));
        let mut received = 0;
        let events = std::iter::from_fn(|| {
            received += 1;
            Some(RuntimeEvent::Device(device::Update::Error(String::new())))
        });
        apply_runtime_batch(events, &mut state, &stopping, &mut dirty);
        assert_eq!(received, 256);
    }

    #[test]
    fn stopping_forgets_the_followed_client_and_refuses_queued_work() {
        use reconcile::tests::{client, daemon};
        let config = crate::config::Config::default();
        let mut state = State::new(config.clone());
        let stopping = Arc::new(AtomicBool::new(false));
        let focus = Focus::new(Arc::clone(&stopping));
        let context = Mutex::new(Arc::new(InputContext::new(&config)));
        let directory =
            Directory::default().updated(vec![daemon("default", vec![client(7, Some(true))])]);
        apply_clients_update(&mut state, directory, &stopping);
        update_input_context(&context, &focus, &state);
        let key = context.lock().unwrap().client.clone().expect("routed");
        assert_eq!(focus.check_focused(&key), Ok(()));

        stop_routing(&mut state, &focus, &context);
        assert!(context.lock().unwrap().client.is_none());
        assert!(stopping.load(Ordering::Acquire));
        for check in [Focus::check_followed, Focus::check_focused] {
            assert_eq!(check(&focus, &key), Err(herdr_device::Refusal::Stopping));
        }
    }

    #[test]
    fn a_focus_loss_is_in_force_before_the_loop_applies_it() {
        use herdr_device::Refusal;
        use reconcile::tests::{client, daemon};
        let config = crate::config::Config::default();
        let mut state = State::new(config.clone());
        let stopping = AtomicBool::new(false);
        let focus = Focus::new(Arc::default());
        let context = Mutex::new(Arc::new(InputContext::new(&config)));
        let status = Arc::new(Mutex::new(serde_json::Value::Null));
        let (events, loop_events) = mpsc::channel();
        let mut dirty = false;
        let listing = |focused| vec![daemon("default", vec![client(7, focused)])];

        // The follower publishes the client with its window in front, and
        // the loop takes a turn.
        let front = Directory::default().updated(listing(Some(true)));
        assert!(clients_changed(&focus, &events, front.clone()));
        apply_runtime_batch(loop_events.try_iter(), &mut state, &stopping, &mut dirty);
        reconcile_dirty(&mut dirty, &mut state, &stopping, &context, &focus, &status);
        let key = context.lock().unwrap().client.clone().expect("routed");
        assert_eq!(focus.check_focused(&key), Ok(()));

        // The window loses focus. The loop has not taken the event, and
        // typing is refused already; jumps are not.
        assert!(clients_changed(
            &focus,
            &events,
            front.updated(listing(Some(false)))
        ));
        assert_eq!(focus.check_focused(&key), Err(Refusal::NotFocused));
        assert_eq!(focus.check_followed(&key), Ok(()));
        // A turn of the loop that publishes the directory it still has
        // does not bring the focus back.
        dirty = true;
        reconcile_dirty(&mut dirty, &mut state, &stopping, &context, &focus, &status);
        assert_eq!(status.lock().unwrap()["clientFocused"], true);
        assert_eq!(focus.check_focused(&key), Err(Refusal::NotFocused));

        // The window is in front again before the loop took either event:
        // the loop still says so, and now the follower does too.
        assert!(clients_changed(&focus, &events, front));
        assert_eq!(focus.check_focused(&key), Ok(()));
        // The client detaches: nothing of it is followed from here on.
        assert!(clients_changed(&focus, &events, Directory::default()));
        assert_eq!(focus.check_followed(&key), Err(Refusal::NotFollowed));
        assert_eq!(loop_events.try_iter().count(), 3);
    }

    #[test]
    fn a_client_behind_another_app_is_refused_once_it_is_no_longer_followed() {
        use herdr_device::Refusal;
        use reconcile::tests::{client, daemon};
        let listing = |clients| vec![daemon("default", clients)];
        // The follower sees another client take focus, or the client detach.
        for newer in [
            listing(vec![client(7, Some(false)), client(8, Some(true))]),
            listing(Vec::new()),
        ] {
            let config = crate::config::Config::default();
            let mut state = State::new(config.clone());
            let stopping = AtomicBool::new(false);
            let focus = Focus::new(Arc::default());
            let context = Mutex::new(Arc::new(InputContext::new(&config)));
            let status = Arc::new(Mutex::new(serde_json::Value::Null));
            let (events, loop_events) = mpsc::channel();
            let mut dirty = false;

            // The loop has published the client with another app in front:
            // its jumps pass, its typing does not.
            let behind = Directory::default().updated(listing(vec![client(7, Some(false))]));
            assert!(clients_changed(&focus, &events, behind.clone()));
            apply_runtime_batch(loop_events.try_iter(), &mut state, &stopping, &mut dirty);
            reconcile_dirty(&mut dirty, &mut state, &stopping, &context, &focus, &status);
            let key = context.lock().unwrap().client.clone().expect("routed");
            assert_eq!(key.client_id, 7);
            assert_eq!(focus.check_followed(&key), Ok(()));
            assert_eq!(focus.check_focused(&key), Err(Refusal::NotFocused));

            // The loop has not taken the event, and a jump captured for the
            // client is refused already, as is typing, for the same reason.
            let newer = behind.updated(newer);
            assert_ne!(newer.followed.as_ref(), Some(&key));
            assert!(clients_changed(&focus, &events, newer));
            for check in [Focus::check_followed, Focus::check_focused] {
                assert_eq!(check(&focus, &key), Err(Refusal::NotFollowed));
            }
            // A turn of the loop that publishes the directory it still has
            // does not bring the client back.
            dirty = true;
            reconcile_dirty(&mut dirty, &mut state, &stopping, &context, &focus, &status);
            assert_eq!(focus.check_followed(&key), Err(Refusal::NotFollowed));
        }
    }

    #[test]
    fn lighting_failure_does_not_revoke_a_herdr_route() {
        let mut state = State::new(crate::config::Config::default());
        state.routing_ready = true;
        apply_runtime_event(
            RuntimeEvent::Device(device::Update::Error("keyboard access denied".into())),
            &mut state,
            &AtomicBool::new(false),
        );
        assert!(state.routing_ready);
        assert_eq!(state.last_device_error, "keyboard access denied");
    }

    #[test]
    fn timestamps_are_utc_iso_8601() {
        assert_eq!(format_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            format_timestamp(UNIX_EPOCH + Duration::from_millis(946_782_245_678)),
            "2000-01-02T03:04:05.678Z"
        );
        assert_eq!(
            format_timestamp(UNIX_EPOCH + Duration::from_secs(1_709_208_000)),
            "2024-02-29T12:00:00.000Z"
        );
    }
}
