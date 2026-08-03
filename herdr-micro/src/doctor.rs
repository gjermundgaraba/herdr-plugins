//! Non-destructive installation checks for the Micro plugin.

use anyhow::Result;
use std::{env, fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command, time::Duration};

use crate::{
    config::{config_path, load_controls, load_effort, load_lighting},
    control::request_status,
    device::{input_monitoring_access, InputMonitoringAccess},
    ghostty::inspect_ghostty,
    macos::{frontmost, post_event_access},
    setup::plugin_root,
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
        Ok(detail) if detail.is_empty() => report.push(Level::Ok, label),
        Ok(detail) => report.push(Level::Ok, format!("{label}: {detail}")),
        Err(error) => report.push(Level::Fail, format!("{label}: {error}")),
    }
}

fn pi_extension() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".pi/agent/extensions/herdr-micro-effort.ts"))
}

pub fn doctor() -> Report {
    let mut report = Report::default();
    check(&mut report, "Rust executable", || {
        let path = plugin_root()?.join("bin/herdr-micro");
        let mode = fs::metadata(&path)?.permissions().mode();
        if mode & 0o111 == 0 {
            anyhow::bail!("{} is not executable", path.display());
        }
        Ok(path.display().to_string())
    });
    match post_event_access() {
        Ok(()) => report.push(Level::Ok, "Scroll event access"),
        Err(_) => report.push(
            Level::Warn,
            "Scroll event access is denied; enable Accessibility for Herdr or its terminal host",
        ),
    }
    match input_monitoring_access() {
        InputMonitoringAccess::Granted => report.push(Level::Ok, "Input Monitoring access"),
        InputMonitoringAccess::Denied => report.push(
            Level::Fail,
            "Input Monitoring is denied; enable it for Herdr or its terminal host",
        ),
        InputMonitoringAccess::Unknown => report.push(
            Level::Warn,
            "Input Monitoring access could not be determined; standard HID key events may be withheld",
        ),
    }
    match frontmost() {
        Ok(current) => report.push(
            Level::Ok,
            format!("Frontmost application: {}", current.app_name),
        ),
        Err(error) => report.push(
            Level::Warn,
            format!("Frontmost application could not be inspected: {error}"),
        ),
    }
    match inspect_ghostty() {
        Ok(ghostty) => report.push(
            Level::Ok,
            format!(
                "Ghostty Automation: {} terminal(s) visible",
                ghostty.terminals.len()
            ),
        ),
        Err(error) => report.push(Level::Fail, format!("Ghostty Automation: {error}")),
    }
    let controls = config_path("controls.json");
    check(&mut report, "control configuration", || {
        load_controls(&controls).map_err(anyhow::Error::msg)?;
        Ok(controls.display().to_string())
    });
    let effort = config_path("effort.json");
    let effort_config = match load_effort(&effort) {
        Ok(config) => {
            report.push(
                Level::Ok,
                format!("effort configuration: {}", effort.display()),
            );
            Some(config)
        }
        Err(error) => {
            report.push(Level::Fail, format!("effort configuration: {error}"));
            None
        }
    };
    let lighting = config_path("lighting.json");
    check(&mut report, "lighting configuration", || {
        load_lighting(&lighting).map_err(anyhow::Error::msg)?;
        Ok(lighting.display().to_string())
    });
    match request_status(Duration::from_millis(750)) {
        Ok(status) => match status.get("device").and_then(serde_json::Value::as_str) {
            Some("connected") => report.push(Level::Ok, "Micro bridge: connected"),
            Some(device) => report.push(Level::Warn, format!("Micro bridge: {device}")),
            None => report.push(Level::Warn, "Micro bridge returned no device status"),
        },
        Err(error) => report.push(Level::Warn, format!("Micro bridge is not running: {error}")),
    }
    if effort_config.as_ref().map_or(true, |config| {
        config.codex.raise.is_none() || config.codex.lower.is_none()
    }) {
        report.push(
            Level::Warn,
            format!("Codex effort shortcuts are unset in {}", effort.display()),
        );
    }
    match pi_extension() {
        Some(path) if path.exists() => report.push(
            Level::Ok,
            format!("Pi effort extension: {}", path.display()),
        ),
        _ => report.push(
            Level::Warn,
            "Pi effort extension is not installed (optional)",
        ),
    }
    match Command::new("hunk").arg("--version").output() {
        Ok(output) if output.status.success() => report.push(Level::Ok, "Hunk diff integration"),
        _ => report.push(
            Level::Warn,
            "Hunk is not installed; the diff button is unavailable",
        ),
    }
    report
}

pub fn run_doctor() -> i32 {
    let report = doctor();
    println!("{}", report.render());
    if report.failed() {
        1
    } else {
        0
    }
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
