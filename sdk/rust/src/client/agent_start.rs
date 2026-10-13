//! `agent.start` that waits for a starting shell, ported from upstream Herdr's
//! CLI (`src/cli/agent.rs`, `agent_start`, v0.9.3). The daemon answers
//! `agent_pane_busy` while a new pane's shell still runs its startup files;
//! the CLI retries that answer for up to two seconds while the pane keeps the
//! same terminal and its shell still leads the foreground job. Here the two
//! seconds also bound the wait for the answer to each read between attempts,
//! so a daemon that stalls on one, or sends it piece by piece, ends the retry
//! with the busy answer.

use std::io::{self, BufReader, Read};
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;
use serde_json::{Value, json};

use super::{
    AgentStartedResult, Client, Error, LocalStream, read_response, set_timeouts, write_request,
};
use crate::{AgentInfo, AgentStartParams};

/// How long a busy answer is retried, counted from the first one.
const SHELL_READINESS_RETRY_TIMEOUT: Duration = Duration::from_secs(2);
const AGENT_START_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PANE_BUSY: &str = "agent_pane_busy";

impl Client {
    /// Starts an agent in an available shell pane and returns once the
    /// daemon launched it; unlike `herdr agent start`, it does not wait for
    /// the agent to become ready. A pane whose shell is still starting is
    /// retried for up to two seconds from the first busy answer, as the CLI
    /// does, and the answer to each read between attempts is awaited only
    /// for what is left of them, however it arrives, where the platform
    /// supports socket timeouts. When the time is up, or a read fails or runs
    /// out of it, the daemon's last `agent_pane_busy` is returned.
    pub fn start_agent(&self, params: &AgentStartParams) -> Result<AgentInfo, Error> {
        self.start_agent_within(params, SHELL_READINESS_RETRY_TIMEOUT)
    }

    fn start_agent_within(
        &self,
        params: &AgentStartParams,
        budget: Duration,
    ) -> Result<AgentInfo, Error> {
        let pane_id = params.pane_id.as_str();
        // Pinned at the first busy answer, so a start that succeeds at once
        // makes no extra call.
        let mut pinned_terminal_id = None;
        let mut retry_deadline = None;
        loop {
            let busy = match self.call::<_, AgentStartedResult>("agent.start", params) {
                Ok(result) => return Ok(result.agent),
                Err(Error::Api(error)) if error.code == PANE_BUSY => Error::Api(error),
                Err(error) => return Err(error),
            };
            let deadline = *retry_deadline.get_or_insert_with(|| Instant::now() + budget);
            let first_busy = pinned_terminal_id.is_none();
            if first_busy {
                pinned_terminal_id = self.pane_terminal_id(pane_id, deadline);
            }
            let Some(pinned) = pinned_terminal_id.as_deref() else {
                return Err(busy);
            };
            // The first busy answer just read the terminal it pins, so only
            // the shell is left to check.
            let starting = if first_busy {
                self.shell_initializing(pane_id, deadline)
            } else {
                self.shell_still_starting(pane_id, pinned, deadline)
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !starting || remaining.is_zero() {
                return Err(busy);
            }
            std::thread::sleep(AGENT_START_POLL_INTERVAL.min(remaining));
            // A read may answer just before its timeout, so the deadline is
            // checked again before another start is sent.
            if !self.shell_still_starting(pane_id, pinned, deadline) || Instant::now() >= deadline {
                return Err(busy);
            }
        }
    }

    /// Whether the pane still holds the pinned terminal and its shell is
    /// still starting. These reads launch nothing, so a failed one only ends
    /// the retry.
    fn shell_still_starting(&self, pane_id: &str, pinned: &str, deadline: Instant) -> bool {
        self.pane_terminal_id(pane_id, deadline).as_deref() == Some(pinned)
            && self.shell_initializing(pane_id, deadline)
    }

    /// Whether the pane's shell is still running its startup files; a
    /// failed read counts as no.
    fn shell_initializing(&self, pane_id: &str, deadline: Instant) -> bool {
        self.read_before("pane.process_info", pane_id, deadline)
            .is_some_and(|result| process_info_shows_shell_initialization(&result["process_info"]))
    }

    /// The pane's terminal id, or `None` when Herdr reports none or the read
    /// fails.
    fn pane_terminal_id(&self, pane_id: &str, deadline: Instant) -> Option<String> {
        // Raw, not `get_pane`: drift in the pane's other fields must not end the retry.
        let result = self.read_before("pane.get", pane_id, deadline)?;
        result["pane"]["terminal_id"].as_str().map(str::to_owned)
    }

    /// A read of the pane whose answer is awaited only until `deadline`.
    /// `None` when the deadline has passed or the read fails, which includes
    /// running out of time.
    fn read_before(&self, method: &str, pane_id: &str, deadline: Instant) -> Option<Value> {
        let timeout = time_left(deadline, self.timeout).ok()?;
        let id = self.request_id();
        let mut stream = self.connect().ok()?;
        set_timeouts(&stream, Some(timeout)).ok()?;
        write_request(&mut stream, &id, method, &json!({ "pane_id": pane_id })).ok()?;
        let answer = DeadlineRead {
            stream,
            deadline,
            limit: self.timeout,
        };
        read_response(&mut BufReader::new(answer), &id).ok()
    }
}

/// What is left until `deadline`, or `limit` when that is shorter. Fails
/// with `TimedOut` once nothing is left, since a zero socket timeout is
/// rejected rather than treated as an instant one.
fn time_left(deadline: Instant, limit: Option<Duration>) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(limit.map_or(remaining, |limit| limit.min(remaining)))
}

/// An answer read against an absolute deadline. A line may take many reads,
/// so each one gets only what is left, and an answer that keeps arriving in
/// pieces still ends at the deadline.
struct DeadlineRead {
    stream: LocalStream,
    deadline: Instant,
    limit: Option<Duration>,
}

impl Read for DeadlineRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let timeout = time_left(self.deadline, self.limit)?;
        // Windows may not support socket timeouts, as `set_timeout_best_effort`
        // allows; there the deadline is checked only before each read.
        if let Err(error) = self.stream.set_recv_timeout(Some(timeout))
            && !(cfg!(windows) && error.kind() == io::ErrorKind::Unsupported)
        {
            return Err(error);
        }
        self.stream.read(buf)
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
