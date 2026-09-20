//! Non-destructive installation checks for the Micro plugin.

use anyhow::Result;
use codex_micro::{
    InputMonitoringAccess,
    service::{Client as DeviceClient, ServiceStatus},
};
use herdr_frontend::{self as frontend, FrontendClient};
use std::{
    env, fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command, sync::mpsc,
    time::Duration,
};

use crate::{
    config::{config_path, load, requires_accessibility},
    control::request_status,
    macos::post_event_access,
    setup::{PI_EXTENSION, plugin_root, service_executable},
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

fn pi_extension() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| PathBuf::from(home).join(PI_EXTENSION))
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
    let paths = frontend::discover(&frontend::directory());
    let snapshots: Vec<_> = paths
        .iter()
        .filter_map(|path| {
            FrontendClient::connect(path)
                .with_timeout(Duration::from_millis(750))
                .snapshot()
                .ok()
        })
        .collect();
    let focused = snapshots.iter().filter(|s| s.focused == Some(true)).count();
    report.push(
        if focused == 1 { Level::Ok } else { Level::Warn },
        format!(
            "Herdr frontends: {} connected; {focused} report focus (exactly one required)",
            snapshots.len()
        ),
    );
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
                        "Codex Micro Input Monitoring: {:?}; run `herdr plugin action invoke \
                         service-authorize --plugin gjermundgaraba.herdr-micro`",
                        status.input_monitoring
                    ),
                );
            }
        }
    }
    check(&mut report, "Thinking-effort adapter", || {
        let path = plugin_root()?.join("integrations/thinking-effort.sh");
        if !fs::metadata(&path)?.is_file() {
            anyhow::bail!("{} is not a file", path.display());
        }
        Ok(path.display().to_string())
    });
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
        match post_event_access() {
            Ok(()) => report.push(Level::Ok, "macOS event output"),
            Err(_) => report.push(
                Level::Fail,
                "macOS event output is denied; enable Accessibility for the process running Herdr",
            ),
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
    match pi_extension() {
        Some(path) if path.exists() => {
            let bundled = plugin_root()
                .and_then(|root| {
                    fs::read(root.join("integrations/pi/herdr-effort.js")).map_err(Into::into)
                })
                .ok();
            match (fs::read(&path), bundled) {
                (Ok(installed), Some(bundled)) if installed == bundled => report.push(
                    Level::Ok,
                    format!("Pi effort extension: {}", path.display()),
                ),
                (Ok(_), Some(_)) => report.push(
                    Level::Warn,
                    format!(
                        "Pi effort extension differs from bundled version: {}",
                        path.display()
                    ),
                ),
                (Err(error), _) => report.push(
                    Level::Warn,
                    format!("Pi effort extension could not be read: {error}"),
                ),
                (_, None) => report.push(Level::Warn, "Bundled Pi effort extension is unavailable"),
            }
        }
        _ => report.push(
            Level::Warn,
            "Pi effort extension is not installed (optional)",
        ),
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
}
