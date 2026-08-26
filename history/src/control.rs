//! Control protocol between invocations and the history daemon: one
//! `command build-hash` line per connection, answered with `ok` or
//! `error <message>`.

use std::{
    fmt,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use herdr_client::{Client, Error as ClientError, unix::peer_is_current_user};
use serde_json::json;

use crate::{
    DAEMON_FLAG, SOCKET_TIMEOUT, START_TIMEOUT, log_error, now_ms, remove_socket, state::State,
};

const ACTIVATE_ACTION: &str = "gjermundgaraba.herdr-history.activate";
const STALE_REPLY: &str = "stale";
const JUMP_BUDGET: Duration = Duration::from_secs(1);

/// The daemon on the control socket was built from different bytes and is
/// retiring; the caller should spawn the current build and retry.
#[derive(Debug)]
pub(crate) struct StaleBuild;

impl fmt::Display for StaleBuild {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("history daemon is from another build")
    }
}

impl std::error::Error for StaleBuild {}

pub(crate) fn activate_daemon(control_socket: &Path, build: &str) -> Result<()> {
    let deadline = Instant::now() + START_TIMEOUT;
    let mut spawned = false;
    while Instant::now() < deadline {
        if let Ok(stream) = UnixStream::connect(control_socket) {
            match send_command(stream, "activate", build) {
                // A daemon built from other bytes retires and unlinks its
                // socket before replying, so the next pass spawns this build.
                Err(error) if error.is::<StaleBuild>() => spawned = false,
                result => return result,
            }
        }
        if !spawned {
            spawn_daemon()?;
            spawned = true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    bail!("history daemon did not start")
}

fn spawn_daemon() -> Result<()> {
    let executable = std::env::current_exe().context("cannot resolve executable")?;
    let mut command = Command::new(executable);
    command
        .arg(DAEMON_FLAG)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command.spawn().context("cannot start history daemon")?;
    Ok(())
}

pub(crate) fn run_active_command(control_socket: &Path, command: &str, build: &str) -> Result<()> {
    let stream = UnixStream::connect(control_socket).map_err(|_| anyhow!(inactive_message()))?;
    match send_command(stream, command, build) {
        // First contact retired a stale daemon and its history with it, so
        // "refuse late history" no longer protects anything: bring up the
        // current build and retry once. Connect refusals still refuse.
        Err(error) if error.is::<StaleBuild>() => {
            activate_daemon(control_socket, build)?;
            let stream =
                UnixStream::connect(control_socket).map_err(|_| anyhow!(inactive_message()))?;
            send_command(stream, command, build)
        }
        result => result,
    }
}

fn inactive_message() -> String {
    format!("history is not active; run `herdr plugin action invoke {ACTIVATE_ACTION}` first")
}

fn send_command(mut stream: UnixStream, command: &str, build: &str) -> Result<()> {
    stream.set_read_timeout(Some(START_TIMEOUT))?;
    writeln!(stream, "{command} {build}").with_context(|| format!("cannot send {command}"))?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .context("cannot read history daemon response")?;
    let response = response.trim();
    if response == "ok" {
        Ok(())
    } else if response.is_empty() || response == format!("error {STALE_REPLY}") {
        // An empty reply means the daemon closed without replying (crashed,
        // or its listener was dropped by a concurrent retire); treat it like
        // a retiring daemon so callers respawn instead of surfacing a
        // truncated protocol error.
        Err(anyhow!(StaleBuild))
    } else if let Some(error) = response.strip_prefix("error ") {
        Err(anyhow!(error.to_owned()))
    } else {
        Err(anyhow!("invalid history daemon response: {response}"))
    }
}

/// Returns false when the daemon must retire because the client runs a
/// different build.
pub(crate) fn handle_control(
    mut stream: UnixStream,
    client: Option<&Client>,
    state: &mut State,
    control_socket: &Path,
    build: &str,
) -> bool {
    let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
    let mut request = String::new();
    let mut retire = None;
    let result = peer_is_current_user(&stream)
        .map_err(|error| format!("cannot authorize history client: {error}"))
        .and_then(|()| {
            BufReader::new(&stream)
                .read_line(&mut request)
                .map_err(|error| format!("cannot read history command: {error}"))
        })
        .and_then(|_| {
            let (command, token) = split_request(&request);
            // A missing token deliberately passes: liveness probes connect
            // and drop without writing anything, so requiring a token here
            // would let a probe retire a healthy daemon.
            if let Some(token) = token
                && token != build
            {
                retire = Some(format!("build {token} replaces {build}"));
                return Err(STALE_REPLY.into());
            }
            match command {
                "activate" => Ok(()),
                "back" => jump(client.ok_or("history is reconnecting")?, state, -1),
                "forward" => jump(client.ok_or("history is reconnecting")?, state, 1),
                command => Err(format!("unknown history command: {command}")),
            }
        });
    // Unlink before replying: the client's next connect must miss this dying
    // daemon and spawn a replacement instead of talking to it.
    if let Some(reason) = &retire {
        log_error(&format!("history daemon retiring: {reason}"));
        let _ = remove_socket(control_socket);
    }
    let _ = match result {
        Ok(()) => writeln!(stream, "ok"),
        Err(error) => writeln!(stream, "error {error}"),
    };
    retire.is_none()
}

fn split_request(request: &str) -> (&str, Option<&str>) {
    let mut fields = request.split_whitespace();
    (fields.next().unwrap_or_default(), fields.next())
}

fn jump(client: &Client, state: &mut State, step: isize) -> Result<(), String> {
    let deadline = Instant::now() + JUMP_BUDGET;
    while Instant::now() < deadline {
        let Some((index, target)) = state.plan_jump(step) else {
            let end = if step < 0 { "oldest" } else { "newest" };
            let _ = client.call_value(
                "notification.show",
                &json!({ "title": format!("history: at {end}") }),
            );
            return Ok(());
        };
        state.push_echo(target.clone(), now_ms());
        match client.focus_pane(&target) {
            Ok(_) => {
                state.cursor = index;
                return Ok(());
            }
            Err(ClientError::Api(error)) if error.code == "pane_not_found" => {
                state.focus_failed(index);
            }
            Err(ClientError::Api(error)) => {
                state.cancel_echo(&target);
                return Err(format!("cannot focus {target}: {error}"));
            }
            Err(error) => return Err(format!("cannot focus {target}: {error}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_command_and_optional_client_version() {
        assert_eq!(split_request("back 0.3.0\n"), ("back", Some("0.3.0")));
        assert_eq!(split_request("activate\n"), ("activate", None));
        assert_eq!(split_request("\n"), ("", None));
    }

    /// Feed one request to `handle_control` over a socketpair, against a
    /// daemon whose build hash is "deadbeef". Returns whether the daemon
    /// keeps running, the trimmed reply, and whether the control socket path
    /// survived (a marker file stands in for the bound socket).
    fn run_control(request: &str, marker_name: &str) -> (bool, String, bool) {
        let marker = std::env::temp_dir().join(format!(
            "herdr-history-ctl-{}-{marker_name}",
            std::process::id()
        ));
        fs::write(&marker, b"").unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        client.write_all(request.as_bytes()).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut state = State::fresh();
        let keep = handle_control(server, None, &mut state, &marker, "deadbeef");
        let mut reply = String::new();
        BufReader::new(&client).read_line(&mut reply).unwrap();
        let socket_kept = marker.exists();
        let _ = fs::remove_file(&marker);
        (keep, reply.trim().to_owned(), socket_kept)
    }

    #[test]
    fn stale_client_retires_daemon_and_unlinks_socket() {
        let (keep, reply, socket_kept) = run_control("back cafef00d\n", "stale");
        assert!(!keep);
        assert_eq!(reply, "error stale");
        assert!(!socket_kept);
    }

    #[test]
    fn probes_and_tokenless_requests_do_not_retire() {
        // A liveness probe connects and closes without writing anything.
        let (keep, reply, socket_kept) = run_control("", "probe");
        assert!(keep);
        assert!(reply.starts_with("error unknown history command"));
        assert!(socket_kept);

        let (keep, reply, socket_kept) = run_control("activate\n", "tokenless");
        assert!(keep);
        assert_eq!(reply, "ok");
        assert!(socket_kept);
    }

    #[test]
    fn matching_build_without_connection_reports_reconnecting() {
        let (keep, reply, socket_kept) = run_control("back deadbeef\n", "reconnect");
        assert!(keep);
        assert_eq!(reply, "error history is reconnecting");
        assert!(socket_kept);
    }

    #[test]
    fn daemon_closing_without_reply_reads_as_stale() {
        let (client, server) = UnixStream::pair().unwrap();
        let reader = std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(&server).read_line(&mut line).unwrap();
            line
        });
        let error = send_command(client, "back", "deadbeef").unwrap_err();
        assert!(error.is::<StaleBuild>());
        assert_eq!(reader.join().unwrap().trim(), "back deadbeef");
    }
}
