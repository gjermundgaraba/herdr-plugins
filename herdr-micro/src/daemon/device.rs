//! Device-service calls run here, independently of input and Herdr routing.

use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use codex_micro::{
    DeviceEvent,
    service::{Client as DeviceClient, Light, Lighting, ServiceStatus, layer_identity},
};

const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Output {
    pub layer: Option<usize>,
    pub lighting: Lighting,
}

pub(super) enum Update {
    Status(ServiceStatus),
    Error(String),
}

#[derive(Default)]
struct Desired(Mutex<Option<Output>>);

impl Desired {
    fn replace(&self, output: Output) -> bool {
        let mut latest = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if latest.as_ref() == Some(&output) {
            return false;
        }
        *latest = Some(output);
        true
    }

    fn latest(&self) -> Option<Output> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

pub(super) struct Worker {
    desired: Arc<Desired>,
    wake: SyncSender<()>,
    stopping: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Worker {
    pub(super) fn spawn(
        event_tx: Sender<DeviceEvent>,
        on_update: impl FnMut(Update) + Send + 'static,
        stopping: Arc<AtomicBool>,
    ) -> Result<Self> {
        let desired = Arc::new(Desired::default());
        let (wake, wake_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("herdr-micro-device".into())
            .spawn({
                let desired = Arc::clone(&desired);
                let stopping = Arc::clone(&stopping);
                move || run(event_tx, desired, wake_rx, on_update, stopping)
            })
            .context("start Micro device worker")?;
        Ok(Self {
            desired,
            wake,
            stopping,
            thread,
        })
    }

    pub(super) fn set_output(&self, output: Output) {
        if self.desired.replace(output) {
            let _ = self.wake.try_send(());
        }
    }

    pub(super) fn shutdown(self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        if self.thread.join().is_err() {
            super::log("device worker panicked");
        }
    }
}

#[derive(Default)]
struct Sent {
    layer: Option<usize>,
    lighting: Option<Lighting>,
    // Includes attempted writes: a failed lighting call may have partly applied.
    all_zones: BTreeSet<String>,
}

impl Sent {
    fn invalidate(&mut self) {
        self.layer = None;
        self.lighting = None;
    }
}

/// Re-read desired state after each blocking call, so a new layer selection
/// takes priority over lighting that became obsolete while selecting a layer.
fn send_pending(
    desired: &Desired,
    sent: &mut Sent,
    stopping: &AtomicBool,
    mut set_layer: impl FnMut(usize) -> Result<()>,
    mut set_lighting: impl FnMut(Lighting) -> Result<()>,
) -> Result<()> {
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let Some(output) = desired.latest() else {
        return Ok(());
    };
    if let Some(layer) = output.layer
        && sent.layer != Some(layer)
    {
        set_layer(layer)?;
        sent.layer = Some(layer);
    }
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let Some(output) = desired.latest() else {
        return Ok(());
    };
    if output.layer.is_some_and(|layer| sent.layer != Some(layer)) {
        return Ok(());
    }
    if sent.lighting.as_ref() != Some(&output.lighting) {
        let mut lighting = output.lighting.clone();
        for zone in &sent.all_zones {
            lighting.aggregate.entry(zone.clone()).or_default();
        }
        sent.all_zones.extend(lighting.aggregate.keys().cloned());
        // A failure may already have changed aggregate lighting. Even returning
        // to the last successful desired output must send its zone clears.
        sent.lighting = None;
        set_lighting(lighting)?;
        sent.lighting = Some(output.lighting);
    }
    Ok(())
}

fn run(
    event_tx: Sender<DeviceEvent>,
    desired: Arc<Desired>,
    wake: Receiver<()>,
    mut on_update: impl FnMut(Update),
    stopping: Arc<AtomicBool>,
) {
    let mut client: Option<DeviceClient> = None;
    let mut acquired = false;
    let mut device_was_connected = false;
    let mut sent = Sent::default();
    let mut retry_due = Instant::now();
    let mut output_due = Instant::now();
    let mut status_due = Instant::now();
    let mut last_status: Option<ServiceStatus> = None;
    let mut last_error = String::new();
    while !stopping.load(Ordering::Acquire) {
        if client.as_ref().is_some_and(DeviceClient::is_closed) {
            if let Some(mut closed) = client.take() {
                let _ = closed.close();
            }
            acquired = false;
            device_was_connected = false;
            sent.invalidate();
            last_status = None;
            retry_due = Instant::now() + RETRY_INTERVAL;
        }
        if client.is_none() && Instant::now() >= retry_due {
            match DeviceClient::connect(event_tx.clone()) {
                Ok(connected) => {
                    client = Some(connected);
                    retry_due = Instant::now();
                    status_due = Instant::now();
                }
                Err(error) => {
                    report_error(
                        &mut on_update,
                        &mut last_error,
                        &mut last_status,
                        "device service connect failed",
                        error,
                    );
                    retry_due = Instant::now() + RETRY_INTERVAL;
                }
            }
        }
        if stopping.load(Ordering::Acquire) {
            break;
        }
        if let Some(device) = client.as_ref() {
            if !acquired && Instant::now() >= retry_due {
                match device.acquire() {
                    Ok(_) => {
                        acquired = true;
                        device_was_connected = true;
                        sent.invalidate();
                        output_due = Instant::now();
                        last_error.clear();
                    }
                    Err(error) => {
                        report_error(
                            &mut on_update,
                            &mut last_error,
                            &mut last_status,
                            "device acquire failed",
                            error,
                        );
                        retry_due = Instant::now() + RETRY_INTERVAL;
                    }
                }
                status_due = Instant::now();
            }
            if stopping.load(Ordering::Acquire) {
                break;
            }
            if acquired && Instant::now() >= output_due {
                match send_pending(
                    &desired,
                    &mut sent,
                    &stopping,
                    |layer| device.set_focused_app(layer_identity(layer)),
                    |lighting| device.set_lighting(lighting),
                ) {
                    Ok(()) => last_error.clear(),
                    Err(error) => {
                        report_error(
                            &mut on_update,
                            &mut last_error,
                            &mut last_status,
                            "device output failed",
                            error,
                        );
                        output_due = Instant::now() + RETRY_INTERVAL;
                    }
                }
            }
            if stopping.load(Ordering::Acquire) {
                break;
            }
            if Instant::now() >= status_due {
                match device.status() {
                    Ok(status) => {
                        if status.device.is_some() && !device_was_connected {
                            sent.invalidate();
                            output_due = Instant::now();
                        }
                        device_was_connected = status.device.is_some();
                        let layer_ready = desired.latest().is_some_and(|output| {
                            output.layer.is_none() || output.layer == sent.layer
                        });
                        if status.device.is_none() || (acquired && layer_ready) {
                            report_status(&mut on_update, &mut last_status, status);
                        }
                    }
                    Err(error) => report_error(
                        &mut on_update,
                        &mut last_error,
                        &mut last_status,
                        "device status failed",
                        error,
                    ),
                }
                status_due = Instant::now() + STATUS_INTERVAL;
            }
            if stopping.load(Ordering::Acquire) {
                break;
            }
        }
        // Signals set the shared stop flag without sending a wakeup.
        let mut wait = STOP_POLL_INTERVAL;
        let now = Instant::now();
        if client.is_none() || !acquired {
            wait = wait.min(retry_due.saturating_duration_since(now));
        }
        if client.is_some() {
            wait = wait.min(status_due.saturating_duration_since(now));
        }
        let _ = wake.recv_timeout(wait);
    }
    if let Some(mut device) = client {
        if acquired {
            let blank = Lighting {
                aggregate: sent
                    .all_zones
                    .into_iter()
                    .map(|zone| (zone, Light::default()))
                    .collect(),
                ..Lighting::default()
            };
            if let Err(error) = device.set_lighting(blank) {
                report_error(
                    &mut on_update,
                    &mut last_error,
                    &mut last_status,
                    "device lighting blank failed",
                    error,
                );
            }
            if let Err(error) = device.set_focused_app(layer_identity(1)) {
                report_error(
                    &mut on_update,
                    &mut last_error,
                    &mut last_status,
                    "safe layer selection failed",
                    error,
                );
            }
        }
        if let Err(error) = device.close() {
            report_error(
                &mut on_update,
                &mut last_error,
                &mut last_status,
                "device service client close failed",
                error,
            );
        }
    }
}

fn report_error(
    on_update: &mut impl FnMut(Update),
    last: &mut String,
    last_status: &mut Option<ServiceStatus>,
    context: &str,
    error: anyhow::Error,
) {
    let message = format!("{context}: {error:#}");
    if *last != message {
        on_update(Update::Error(message.clone()));
        *last = message;
        // The next eligible status must clear this diagnostic in the daemon,
        // even when the service status itself has not changed.
        *last_status = None;
    }
}

fn report_status(
    on_update: &mut impl FnMut(Update),
    last: &mut Option<ServiceStatus>,
    status: ServiceStatus,
) {
    if last.as_ref() != Some(&status) {
        on_update(Update::Status(status.clone()));
        *last = Some(status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(layer: usize, zone: &str) -> Output {
        Output {
            layer: Some(layer),
            lighting: Lighting {
                aggregate: [(
                    zone.into(),
                    Light {
                        c: 0x123456,
                        b: 0.5,
                        ..Light::default()
                    },
                )]
                .into(),
                ..Lighting::default()
            },
        }
    }

    #[test]
    fn repeated_output_is_suppressed_and_pending_output_is_replaced() {
        let desired = Desired::default();
        assert!(desired.replace(output(1, "old")));
        assert!(!desired.replace(output(1, "old")));
        assert!(desired.replace(output(2, "new")));
        assert_eq!(desired.latest(), Some(output(2, "new")));
    }

    #[test]
    fn new_layer_during_blocked_send_skips_obsolete_lighting() {
        let desired = Arc::new(Desired::default());
        desired.replace(output(1, "old"));
        let (entered_tx, entered_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let worker_desired = Arc::clone(&desired);
        let sender = thread::spawn(move || {
            let mut sent = Sent::default();
            send_pending(
                &worker_desired,
                &mut sent,
                &AtomicBool::new(false),
                |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
                |_| panic!("obsolete lighting must not be sent"),
            )
            .unwrap();
            sent
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        desired.replace(output(2, "intermediate"));
        desired.replace(output(3, "latest"));
        release_tx.send(()).unwrap();
        let mut sent = sender.join().unwrap();
        let mut layers = Vec::new();
        let mut lights = Vec::new();
        send_pending(
            &desired,
            &mut sent,
            &AtomicBool::new(false),
            |layer| {
                layers.push(layer);
                Ok(())
            },
            |lighting| {
                lights.push(lighting);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(layers, vec![3]);
        assert_eq!(lights, vec![output(3, "latest").lighting]);
    }

    #[test]
    fn omitted_zones_are_cleared_even_when_intermediate_output_is_coalesced() {
        let desired = Desired::default();
        let mut sent = Sent::default();
        let stopping = AtomicBool::new(false);
        desired.replace(output(1, "old"));
        send_pending(&desired, &mut sent, &stopping, |_| Ok(()), |_| Ok(())).unwrap();
        desired.replace(output(1, "intermediate"));
        desired.replace(output(1, "latest"));
        let mut lights = Vec::new();
        send_pending(
            &desired,
            &mut sent,
            &stopping,
            |_| panic!("layer was already sent"),
            |lighting| {
                lights.push(lighting);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(lights[0].aggregate["old"], Light::default());
        assert!(!lights[0].aggregate.contains_key("intermediate"));
        assert_eq!(
            sent.all_zones,
            BTreeSet::from(["old".into(), "latest".into()])
        );
        send_pending(
            &desired,
            &mut sent,
            &stopping,
            |_| panic!("duplicate layer"),
            |_| panic!("duplicate lighting"),
        )
        .unwrap();
    }

    #[test]
    fn partially_failed_lighting_is_cleared_when_returning_to_previous_output() {
        let desired = Desired::default();
        let mut sent = Sent::default();
        let stopping = AtomicBool::new(false);
        desired.replace(output(1, "old"));
        send_pending(&desired, &mut sent, &stopping, |_| Ok(()), |_| Ok(())).unwrap();

        desired.replace(output(1, "partially-enabled"));
        let result = send_pending(
            &desired,
            &mut sent,
            &stopping,
            |_| panic!("layer was already sent"),
            |lighting| {
                assert_eq!(lighting.aggregate["partially-enabled"].c, 0x123456);
                anyhow::bail!("aggregate write applied, slot write failed")
            },
        );
        assert!(result.is_err());

        desired.replace(output(1, "old"));
        let mut lights = Vec::new();
        send_pending(
            &desired,
            &mut sent,
            &stopping,
            |_| panic!("layer was already sent"),
            |lighting| {
                lights.push(lighting);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(lights.len(), 1);
        assert_eq!(lights[0].aggregate["partially-enabled"], Light::default());
        assert_eq!(lights[0].aggregate["old"].c, 0x123456);
    }
    #[test]
    fn unchanged_healthy_status_is_republished_once_after_an_error() {
        let status = ServiceStatus {
            device: Some(codex_micro::DeviceInfo {
                transport: codex_micro::Transport::Usb,
                firmware: "test".into(),
            }),
            external_owner: None,
            last_error: None,
            input_monitoring: codex_micro::InputMonitoringAccess::Granted,
        };
        let mut last_status = None;
        let mut last_error = String::new();
        let mut updates = Vec::new();
        let mut on_update = |update| updates.push(update);
        report_status(&mut on_update, &mut last_status, status.clone());
        report_error(
            &mut on_update,
            &mut last_error,
            &mut last_status,
            "device output failed",
            anyhow::anyhow!("temporary failure"),
        );
        report_error(
            &mut on_update,
            &mut last_error,
            &mut last_status,
            "device output failed",
            anyhow::anyhow!("temporary failure"),
        );
        // Successful output clears the worker's error before the next poll.
        last_error.clear();
        report_status(&mut on_update, &mut last_status, status.clone());
        report_status(&mut on_update, &mut last_status, status.clone());
        assert_eq!(updates.len(), 3);
        assert!(matches!(&updates[0], Update::Status(value) if value == &status));
        assert!(matches!(&updates[1], Update::Error(_)));
        assert!(matches!(&updates[2], Update::Status(value) if value == &status));
    }
}
