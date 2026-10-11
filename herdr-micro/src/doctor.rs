//! Non-destructive installation checks for the Micro plugin.

use anyhow::Result;
use codex_micro::{
    InputMonitoringAccess,
    service::{Client as DeviceClient, ServiceStatus},
};
use herdr_client::{
    follow::{self, Directory},
    sessions,
};
use std::{fs, os::unix::fs::PermissionsExt, process::Command, sync::mpsc, time::Duration};

use crate::{
    config::{config_path, load, requires_accessibility},
    control::request_status,
    setup::service_executable,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub level: Level,
    pub message: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    pub entries: Vec<Entry>,
}

impl Report {
    pub fn push(&mut self, level: Level, message: impl Into<String>) {
        self.entries.push(Entry {
            level,
            message: message.into(),
        });
    }
    pub fn failed(&self) -> bool {
        self.entries.iter().any(|entry| entry.level == Level::Fail)
    }
    pub fn render(&self) -> String {
        self.entries
            .iter()
            .map(|entry| format!("[{}] {}", entry.level.label(), entry.message))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn check(report: &mut Report, label: &str, f: impl FnOnce() -> Result<String>) {
    match f() {
        Ok(detail) => report.push(Level::Ok, format!("{label}: {detail}")),
        Err(error) => report.push(Level::Fail, format!("{label}: {error}")),
    }
}

/// What the Herdr daemons say about their clients right now. One reading
/// has no history, so the client the bridge follows is in its own status
/// ([`report_followed`]).
fn report_clients(report: &mut Report, directory: &Directory) {
    let reporting = directory
        .daemons
        .iter()
        .flat_map(|daemon| &daemon.clients)
        .filter(|client| client.report.is_some())
        .count();
    let (level, focus) = match directory.followed_client() {
        Some(client) if client.has_focus() => (
            Level::Ok,
            format!(
                "client {} of session {} has focus",
                client.client_id, client.session
            ),
        ),
        Some(client) => (
            Level::Ok,
            format!(
                "none has focus now; the keypad follows the client that last took it, or client {} of session {} if none did since the bridge started",
                client.client_id, client.session
            ),
        ),
        None => (Level::Warn, "the keypad has no client to follow".to_owned()),
    };
    report.push(
        level,
        format!(
            "Herdr clients: {} attached across {} session(s), {reporting} reporting; {focus}",
            directory.client_count(),
            directory.daemons.len()
        ),
    );
}

/// Which client the running bridge follows, from its status.
fn report_followed(report: &mut Report, status: &serde_json::Value) {
    let client = status.get("client").and_then(serde_json::Value::as_u64);
    let session = status.get("session").and_then(serde_json::Value::as_str);
    let (Some(client), Some(session)) = (client, session) else {
        report.push(
            Level::Warn,
            "Micro bridge has no Herdr client to follow: none has sent a report, or the followed client's daemon is not answering",
        );
        return;
    };
    let focused = status.get("clientFocused") == Some(&serde_json::Value::Bool(true));
    report.push(
        Level::Ok,
        format!(
            "Micro bridge follows Herdr client {client} of session {session}; {}",
            if focused {
                "its window has focus"
            } else {
                "its window does not report focus, so Agent keys jump and prompts and Enter are refused"
            }
        ),
    );
}

fn service_status() -> Result<ServiceStatus> {
    let (events, _event_rx) = mpsc::channel();
    let mut client = DeviceClient::connect(events)?;
    let status = client.status();
    let close = client.close();
    match (status, close) {
        (Ok(status), Ok(())) => Ok(status),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

pub fn doctor() -> Report {
    let mut report = Report::default();
    match sessions::discover() {
        Ok(sessions) => {
            let directory =
                follow::list_once(&sessions, Duration::from_millis(750), |session, error| {
                    report.push(
                        Level::Warn,
                        format!("Herdr session {}: {error}", session.name),
                    )
                });
            report_clients(&mut report, &directory);
        }
        Err(error) => report.push(Level::Warn, format!("Herdr sessions: {error}")),
    }
    check(&mut report, "Codex Micro service executable", || {
        let path = service_executable()?;
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() {
            anyhow::bail!("{} is not a file", path.display());
        }
        let mode = metadata.permissions().mode();
        if mode & 0o111 == 0 {
            anyhow::bail!("{} is not executable", path.display());
        }
        Ok(path.display().to_string())
    });
    let service_status = match service_status() {
        Ok(status) => {
            let detail = if let Some(owner) = status.external_owner {
                format!("running; device owned by {owner}")
            } else {
                status.device.as_ref().map_or_else(
                    || {
                        status
                            .last_error
                            .as_ref()
                            .map_or_else(|| "running".into(), |error| format!("running; {error}"))
                    },
                    |device| {
                        format!(
                            "running; transport={:?}; firmware={}",
                            device.transport, device.firmware
                        )
                    },
                )
            };
            report.push(Level::Ok, format!("Codex Micro service IPC: {detail}"));
            Some(status)
        }
        Err(error) => {
            report.push(Level::Fail, format!("Codex Micro service IPC: {error}"));
            None
        }
    };
    if let Some(status) = service_status {
        match status.input_monitoring {
            InputMonitoringAccess::Granted => {
                report.push(Level::Ok, "Codex Micro Input Monitoring: granted")
            }
            InputMonitoringAccess::Denied | InputMonitoringAccess::Unknown => {
                report.push(
                    Level::Fail,
                    format!(
                        "Codex Micro Input Monitoring: {:?}; run `herdr plugin invoke \
                         gjermundgaraba.herdr-micro.service-authorize`",
                        status.input_monitoring
                    ),
                );
            }
        }
    }
    let resolved_config_path = config_path();
    let config = match &resolved_config_path {
        Ok(path) => match load(path) {
            Ok(config) => {
                report.push(Level::Ok, format!("configuration: {}", path.display()));
                Some(config)
            }
            Err(error) => {
                report.push(Level::Fail, format!("configuration: {error}"));
                None
            }
        },
        Err(error) => {
            report.push(Level::Fail, format!("configuration: {error}"));
            None
        }
    };
    if config
        .as_ref()
        .is_some_and(|config| requires_accessibility(&config.controls))
    {
        if herdr_device::macos::permitted() {
            report.push(Level::Ok, "macOS event output");
        } else {
            report.push(
                Level::Fail,
                "macOS event output is denied, so prompts, Enter, and key taps cannot be sent; enable Accessibility for the process running Herdr",
            );
        }
    }
    match request_status(Duration::from_millis(750)) {
        Ok(status) => {
            if let Some(error) = status.get("error").and_then(serde_json::Value::as_str) {
                report.push(Level::Warn, format!("Micro bridge: {error}"));
            } else {
                match status.get("device").and_then(serde_json::Value::as_str) {
                    Some("connected") => report.push(Level::Ok, "Micro bridge: connected"),
                    Some(device) => report.push(Level::Warn, format!("Micro bridge: {device}")),
                    None => report.push(Level::Warn, "Micro bridge returned no device status"),
                }
                report_followed(&mut report, &status);
                if let Some(error) = status
                    .get("deviceError")
                    .and_then(serde_json::Value::as_str)
                {
                    report.push(Level::Warn, format!("Micro device runtime error: {error}"));
                }
            }
        }
        // A stopped bridge is a normal state (micro-stop),
        // not an installation failure.
        Err(error) => report.push(Level::Warn, format!("Micro bridge is not running: {error}")),
    }
    match Command::new("hunk").arg("--version").output() {
        Ok(output) if output.status.success() => report.push(Level::Ok, "Hunk diff integration"),
        _ => report.push(
            Level::Warn,
            "Hunk is not installed; the diff popup pane is unavailable",
        ),
    }
    report
}

pub fn run_doctor() -> i32 {
    let report = doctor();
    println!("{}", report.render());
    if report.failed() { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_aggregation_only_fails_for_failures() {
        let mut report = Report::default();
        report.push(Level::Ok, "one");
        report.push(Level::Warn, "two");
        assert!(!report.failed());
        report.push(Level::Fail, "three");
        assert!(report.failed());
        assert_eq!(report.render(), "[ok] one\n[warn] two\n[fail] three");
    }

    #[test]
    fn clients_are_reported_with_the_one_that_has_focus_now() {
        use herdr_client::{
            ClientInfo,
            follow::{DaemonClients, DaemonState},
        };
        let client = |client_id, focused| {
            let mut report = crate::protocol::tests::fixture_report();
            report.focused = focused;
            ClientInfo {
                client_id,
                report: Some(report),
            }
        };
        let daemon = |session: &str, state, clients| DaemonClients {
            session: session.into(),
            socket_path: format!("/run/{session}.sock").into(),
            state,
            clients,
        };
        let following = || DaemonState::Following {
            boot_id: "boot".into(),
        };
        let render = |daemons| {
            let mut report = Report::default();
            report_clients(&mut report, &Directory::default().updated(daemons));
            report.render()
        };
        assert_eq!(
            render(vec![
                daemon("default", following(), vec![client(1, Some(false))]),
                daemon("work", following(), vec![client(4, Some(true))]),
            ]),
            "[ok] Herdr clients: 2 attached across 2 session(s), 2 reporting; client 4 of session work has focus"
        );
        // Another app is in front: nothing to fix.
        assert_eq!(
            render(vec![daemon(
                "default",
                following(),
                vec![
                    client(1, Some(false)),
                    ClientInfo {
                        client_id: 2,
                        report: None
                    }
                ]
            )]),
            "[ok] Herdr clients: 2 attached across 1 session(s), 1 reporting; none has focus now; the keypad follows the client that last took it, or client 1 of session default if none did since the bridge started"
        );
        // No client has sent a report, or no daemon could be listed.
        for (daemons, counts) in [
            (
                vec![daemon(
                    "default",
                    following(),
                    vec![ClientInfo {
                        client_id: 2,
                        report: None,
                    }],
                )],
                "1 attached across 1 session(s), 0 reporting",
            ),
            (
                vec![
                    daemon("default", DaemonState::Offline, vec![]),
                    daemon("work", DaemonState::Offline, vec![]),
                ],
                "0 attached across 2 session(s), 0 reporting",
            ),
        ] {
            assert_eq!(
                render(daemons),
                format!("[warn] Herdr clients: {counts}; the keypad has no client to follow")
            );
        }
    }

    #[test]
    fn the_bridge_status_says_which_client_is_followed() {
        let render = |status: serde_json::Value| {
            let mut report = Report::default();
            report_followed(&mut report, &status);
            report.render()
        };
        assert_eq!(
            render(serde_json::json!({"session": "work", "client": 7, "clientFocused": true})),
            "[ok] Micro bridge follows Herdr client 7 of session work; its window has focus"
        );
        assert_eq!(
            render(serde_json::json!({"session": "work", "client": 7, "clientFocused": false})),
            "[ok] Micro bridge follows Herdr client 7 of session work; its window does not report focus, so Agent keys jump and prompts and Enter are refused"
        );
        assert_eq!(
            render(serde_json::json!({"session": null, "client": null, "clientFocused": null})),
            "[warn] Micro bridge has no Herdr client to follow: none has sent a report, or the followed client's daemon is not answering"
        );
    }
}
