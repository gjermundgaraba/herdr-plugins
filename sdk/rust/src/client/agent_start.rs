//! `agent.start` that waits for a starting shell, ported from upstream Herdr's
//! CLI (`src/cli/agent.rs`, `agent_start`, v0.9.3). The daemon answers
//! `agent_pane_busy` while a new pane's shell still runs its startup files;
//! the CLI retries that answer for up to two seconds while the pane keeps the
//! same terminal and its shell still leads the foreground job.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{AgentStartedResult, Client, Error};
use crate::{AgentInfo, AgentStartParams};

/// How long a busy answer is retried, counted from the first one.
const SHELL_READINESS_RETRY_TIMEOUT: Duration = Duration::from_secs(2);
const AGENT_START_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// The daemon's default and accepted `agent.start` timeouts. The CLI retries
/// only a start the daemon would accept.
const DEFAULT_AGENT_START_TIMEOUT_MS: u64 = 30_000;
const AGENT_START_SETTLE_DELAY_MS: u64 = 3_000;
const MAX_AGENT_START_TIMEOUT_MS: u64 = 300_000;
const PANE_BUSY: &str = "agent_pane_busy";

impl Client {
    /// Starts an agent in an available shell pane. A pane whose shell is
    /// still starting is retried for up to two seconds, as `herdr agent
    /// start` does; after that the daemon's last `agent_pane_busy` is
    /// returned.
    pub fn start_agent(&self, params: &AgentStartParams) -> Result<AgentInfo, Error> {
        let timeout_ms = params.timeout_ms.unwrap_or(DEFAULT_AGENT_START_TIMEOUT_MS);
        let retryable_timeout =
            timeout_ms > AGENT_START_SETTLE_DELAY_MS && timeout_ms <= MAX_AGENT_START_TIMEOUT_MS;
        let pane_id = params.pane_id.as_str();
        let pinned_terminal_id = self.pane_terminal_id(pane_id)?;
        let mut retry_deadline = None;
        loop {
            let busy = match self.call::<_, AgentStartedResult>("agent.start", params) {
                Ok(result) => return Ok(result.agent),
                Err(Error::Api(error)) if error.code == PANE_BUSY => error,
                Err(error) => return Err(error),
            };
            if !retryable_timeout
                || pinned_terminal_id.is_none()
                || !self.shell_still_starting(pane_id, pinned_terminal_id.as_deref())?
            {
                return Err(Error::Api(busy));
            }
            let deadline = *retry_deadline
                .get_or_insert_with(|| Instant::now() + SHELL_READINESS_RETRY_TIMEOUT);
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Api(busy));
            }
            std::thread::sleep(AGENT_START_POLL_INTERVAL.min(remaining));
            if Instant::now() >= deadline
                || !self.shell_still_starting(pane_id, pinned_terminal_id.as_deref())?
            {
                return Err(Error::Api(busy));
            }
        }
    }

    /// Whether the pane still holds the pinned terminal and its shell is
    /// still starting.
    fn shell_still_starting(&self, pane_id: &str, pinned: Option<&str>) -> Result<bool, Error> {
        if self.pane_terminal_id(pane_id)?.as_deref() != pinned {
            return Ok(false);
        }
        let process_info =
            match self.call_value("pane.process_info", &json!({ "pane_id": pane_id })) {
                Ok(result) => result["process_info"].clone(),
                Err(Error::Api(_)) => Value::Null,
                Err(error) => return Err(error),
            };
        Ok(process_info_shows_shell_initialization(&process_info))
    }

    /// The pane's terminal id, or `None` when Herdr reports none, as the CLI
    /// reads it.
    fn pane_terminal_id(&self, pane_id: &str) -> Result<Option<String>, Error> {
        match self.call_value("pane.get", &json!({ "pane_id": pane_id })) {
            Ok(result) => Ok(result["pane"]["terminal_id"].as_str().map(str::to_owned)),
            Err(Error::Api(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// The shell leads the foreground job, so whatever else runs in it is a
/// helper its startup files started.
#[cfg(unix)]
fn process_info_shows_shell_initialization(process_info: &Value) -> bool {
    let Some(shell_pid) = process_info["shell_pid"].as_u64() else {
        return false;
    };
    if process_info["foreground_process_group_id"].as_u64() != Some(shell_pid) {
        return false;
    }
    process_info["foreground_processes"]
        .as_array()
        .is_some_and(|processes| {
            processes.iter().any(|process| {
                process["pid"].as_u64() == Some(shell_pid)
                    && (process["name"]
                        .as_str()
                        .is_some_and(is_pane_shell_process_name)
                        || process["argv"]
                            .as_array()
                            .and_then(|argv| argv.first())
                            .and_then(Value::as_str)
                            .is_some_and(is_pane_shell_process_name))
            })
        })
}

// Windows exposes no foreground process group, so shell initialization is not
// observable and a busy start is not retried there.
#[cfg(not(unix))]
fn process_info_shows_shell_initialization(_process_info: &Value) -> bool {
    false
}

/// Herdr's `platform::is_pane_shell_process_name`.
#[cfg(unix)]
fn is_pane_shell_process_name(name: &str) -> bool {
    let normalized = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "ksh"
            | "mksh"
            | "csh"
            | "tcsh"
            | "elvish"
            | "xonsh"
            | "nu"
            | "pwsh"
            | "powershell"
            | "cmd"
    )
}

#[cfg(all(test, unix))]
mod tests;
