//! The long-lived Codex Micro bridge.  Keep the policy here; HID framing,
//! Herdr parsing, gestures, and macOS operations live in their small modules.
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
    hub,
};
use dispatch::{action_worker, input_worker};
use reconcile::{
    InputContext, State, apply_config_load, apply_hub_update, apply_service_status, device_output,
    handle_input_disconnect, publish, reconcile_active, refresh_owner, update_input_context,
};

const CONFIG_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const OWNER_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
const WORK_QUEUE_CAPACITY: usize = 16;
pub const DAEMON_PROTOCOL_VERSION: u32 = 10;

static LOG_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

enum RuntimeEvent {
    Hub(Box<hub::Update>),
    InputDisconnected(String),
    Device(device::Update),
}

fn apply_runtime_event(event: RuntimeEvent, state: &mut State, stopping: &AtomicBool) {
    match event {
        RuntimeEvent::Hub(update) => apply_hub_update(state, *update, stopping),
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
    hub::spawn_updates({
        let runtime_tx = runtime_tx.clone();
        move |update| runtime_tx.send(RuntimeEvent::Hub(Box::new(update))).is_ok()
    });
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
        let routing_generation = Arc::clone(&state.routing_generation);
        let active_route = Arc::clone(&state.active_route);
        let stopping = Arc::clone(&stopping);
        move || action_worker(work_rx, routing_generation, active_route, stopping)
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
                runtime_tx,
                stopping,
            )
        }
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
            refresh_owner(&mut state);
        }
        if now >= config_due {
            config_due = now + CONFIG_REFRESH_INTERVAL;
            apply_config_load(&mut state, load(&config_path), startup_enabled_buttons);
        }
        reconcile_active(&mut state, &stopping);
        update_input_context(&input_context, &state);
        publish(&status, &state);
        device.set_output(device_output(&state));

        let deadline = config_due.min(owner_due);
        match runtime_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(event) => {
                // Apply transitions in order so even a focus round-trip revokes
                // captured actions. Publish only the final output in this batch.
                // Bound each batch to keep owner/config checks responsive.
                for event in std::iter::once(event).chain(runtime_rx.try_iter().take(255)) {
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    apply_runtime_event(event, &mut state, &stopping);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    log("stopping");
    state.revoke_routing();
    stopping.store(true, Ordering::Release);
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
    fn lighting_failure_does_not_revoke_a_herdr_route() {
        let mut state = State::new(crate::config::Config::default());
        state.routing_ready = true;
        let generation = state.routing_generation();
        apply_runtime_event(
            RuntimeEvent::Device(device::Update::Error("keyboard access denied".into())),
            &mut state,
            &AtomicBool::new(false),
        );
        assert!(state.routing_ready);
        assert_eq!(state.routing_generation(), generation);
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
