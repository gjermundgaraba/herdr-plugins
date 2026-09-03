//! Herdr session discovery and direct socket access.

use anyhow::{Context, Result, bail};
use herdr_client::{AgentInfo, Client, SessionSnapshot};
use serde::Deserialize;
use std::{
    env,
    io::{self, Read},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread,
    time::{Duration, Instant},
};

pub const DEFAULT_HERDR_BIN: &str = "herdr";
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_OUTPUT_LIMIT: usize = 64 * 1024;
const MIN_HERDR_PROTOCOL: u32 = 20;
const SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    pub socket_path: PathBuf,
}

impl Session {
    pub fn client(&self) -> Client {
        Client::new(&self.socket_path).with_timeout(COMMAND_TIMEOUT)
    }
}

#[derive(Debug)]
pub enum SessionUpdate {
    Agents {
        session: String,
        generation: u64,
        agents: Vec<AgentInfo>,
        client_focused: Option<bool>,
    },
    Unavailable {
        session: String,
        generation: u64,
        error: String,
    },
}

pub struct SessionWorker {
    pub generation: u64,
    active: Arc<AtomicBool>,
    refresh: Arc<AtomicBool>,
    thread: thread::Thread,
}

impl SessionWorker {
    /// Resend the next snapshot even if nothing changed.
    pub fn refresh(&self) {
        self.refresh.store(true, Ordering::Release);
        self.thread.unpark();
    }

    #[cfg(test)]
    pub fn refresh_requested(&self) -> bool {
        self.refresh.load(Ordering::Acquire)
    }
}

impl Drop for SessionWorker {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.thread.unpark();
    }
}

pub fn spawn_session_worker(
    session: Session,
    generation: u64,
    updates: Sender<SessionUpdate>,
    stopping: Arc<AtomicBool>,
) -> SessionWorker {
    let active = Arc::new(AtomicBool::new(true));
    let worker_active = Arc::clone(&active);
    let refresh = Arc::new(AtomicBool::new(false));
    let worker_refresh = Arc::clone(&refresh);
    let worker = thread::spawn(move || {
        let mut retry = Duration::from_millis(250);
        while worker_active.load(Ordering::Acquire) && !stopping.load(Ordering::Acquire) {
            let mut connected = false;
            let result = follow_session(
                &session,
                generation,
                &updates,
                &worker_active,
                &worker_refresh,
                &stopping,
                &mut connected,
            );
            if !worker_active.load(Ordering::Acquire) || stopping.load(Ordering::Acquire) {
                break;
            }
            let _ = updates.send(SessionUpdate::Unavailable {
                session: session.name.clone(),
                generation,
                error: result
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "Herdr snapshot polling ended".into()),
            });
            if connected {
                retry = Duration::from_millis(250);
            }
            thread::park_timeout(retry);
            retry = (retry * 2).min(Duration::from_secs(5));
        }
    });
    let thread = worker.thread().clone();
    drop(worker);
    SessionWorker {
        generation,
        active,
        refresh,
        thread,
    }
}

fn follow_session(
    session: &Session,
    generation: u64,
    updates: &Sender<SessionUpdate>,
    active: &AtomicBool,
    refresh: &AtomicBool,
    stopping: &AtomicBool,
    connected: &mut bool,
) -> Result<()> {
    if !active.load(Ordering::Acquire) || stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let client = session.client();
    let mut previous = None;
    loop {
        let snapshot = current_snapshot(&client)?;
        let next = (snapshot.client_focused, snapshot.agents);
        if refresh.swap(false, Ordering::AcqRel) {
            previous = None;
        }
        if previous.as_ref() != Some(&next) {
            send_agents(&session.name, generation, updates, next.1.clone(), next.0)?;
            previous = Some(next);
        }
        *connected = true;
        thread::park_timeout(SNAPSHOT_POLL_INTERVAL);
        if !active.load(Ordering::Acquire) || stopping.load(Ordering::Acquire) {
            return Ok(());
        }
    }
}

pub(crate) fn current_snapshot(client: &Client) -> Result<SessionSnapshot> {
    validate_snapshot(client.snapshot()?)
}

fn validate_snapshot(snapshot: SessionSnapshot) -> Result<SessionSnapshot> {
    if snapshot.protocol < MIN_HERDR_PROTOCOL {
        bail!(
            "Herdr protocol {} is unsupported; expected at least {}",
            snapshot.protocol,
            MIN_HERDR_PROTOCOL
        );
    }
    if snapshot
        .panes
        .iter()
        .any(|pane| pane.pane_id.is_empty() || pane.terminal_id.is_empty())
        || snapshot
            .agents
            .iter()
            .any(|agent| agent.pane_id.is_empty() || agent.terminal_id.is_empty())
    {
        bail!("Herdr returned empty pane or terminal identity");
    }
    Ok(snapshot)
}

fn send_agents(
    session: &str,
    generation: u64,
    updates: &Sender<SessionUpdate>,
    agents: Vec<AgentInfo>,
    client_focused: Option<bool>,
) -> Result<()> {
    updates
        .send(SessionUpdate::Agents {
            session: session.into(),
            generation,
            agents,
            client_focused,
        })
        .map_err(|_| anyhow::anyhow!("Micro daemon stopped"))
}

pub fn herdr_bin() -> String {
    env::var_os("HERDR_BIN_PATH")
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_HERDR_BIN.to_owned())
}

pub fn discover_sessions() -> Result<Vec<Session>> {
    let mut command = Command::new(herdr_bin());
    command
        .args(["session", "list", "--json"])
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_TAB_ID")
        .env_remove("HERDR_WORKSPACE_ID");
    parse_sessions(&run_command_with_timeout(&mut command, COMMAND_TIMEOUT)?)
}

fn parse_sessions(output: &str) -> Result<Vec<Session>> {
    #[derive(Deserialize)]
    struct RawSession {
        name: String,
        running: bool,
        socket_path: PathBuf,
    }
    #[derive(Deserialize)]
    struct Sessions {
        sessions: Vec<RawSession>,
    }

    let sessions: Sessions =
        serde_json::from_str(output).context("Herdr returned invalid session list")?;
    let mut sessions: Vec<_> = sessions
        .sessions
        .into_iter()
        .filter(|session| session.running)
        .map(|session| {
            if session.name.is_empty() || session.socket_path.as_os_str().is_empty() {
                bail!("Herdr returned invalid session list");
            }
            Ok(Session {
                name: session.name,
                socket_path: session.socket_path,
            })
        })
        .collect::<Result<_>>()?;
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

#[derive(Default)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn capture_output(mut reader: impl Read) -> io::Result<CapturedOutput> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((COMMAND_OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > COMMAND_OUTPUT_LIMIT;
    bytes.truncate(COMMAND_OUTPUT_LIMIT);
    io::copy(&mut reader, &mut io::sink())?;
    Ok(CapturedOutput { bytes, truncated })
}

fn join_output(
    reader: thread::JoinHandle<io::Result<CapturedOutput>>,
    stream: &str,
    program: &str,
) -> Result<CapturedOutput> {
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("{stream} reader for {program} panicked"))?
        .with_context(|| format!("read {stream} from {program}"))
}

fn terminate_process_group(process_group: i32) -> std::io::Result<()> {
    if unsafe { libc::kill(-process_group, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

pub(crate) fn run_command_with_timeout(command: &mut Command, timeout: Duration) -> Result<String> {
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run {program}"))?;
    let process_group = match i32::try_from(child.id()) {
        Ok(process_group) => process_group,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("child process ID is too large");
        }
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = thread::spawn(move || capture_output(stdout));
    let stderr_reader = thread::spawn(move || capture_output(stderr));
    let started = Instant::now();
    let waited = (|| -> Result<_> {
        loop {
            if let Some(status) = child
                .try_wait()
                .with_context(|| format!("wait for {program}"))?
            {
                break Ok((status, false));
            }
            if started.elapsed() >= timeout {
                terminate_process_group(process_group)
                    .with_context(|| format!("terminate timed out {program}"))?;
                let status = child
                    .wait()
                    .with_context(|| format!("reap timed out {program}"))?;
                break Ok((status, true));
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if waited.is_err() {
        let _ = terminate_process_group(process_group);
        let _ = child.kill();
        let _ = child.wait();
    }
    let (status, timed_out) = waited?;
    terminate_process_group(process_group)
        .with_context(|| format!("terminate descendants of {program}"))?;
    let captured_stdout = join_output(stdout_reader, "stdout", &program);
    let captured_stderr = join_output(stderr_reader, "stderr", &program);
    let captured_stdout = captured_stdout?;
    let captured_stderr = captured_stderr?;
    let detail = String::from_utf8_lossy(&captured_stderr.bytes)
        .trim()
        .to_owned();
    let truncated = if captured_stderr.truncated {
        " (stderr truncated)"
    } else {
        ""
    };
    if timed_out {
        bail!("{program} timed out after {}s", timeout.as_secs_f64());
    }
    if !status.success() {
        bail!("{program} failed with {status}: {detail}{truncated}");
    }
    Ok(String::from_utf8_lossy(&captured_stdout.bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::{UnixListener, UnixStream},
        sync::mpsc,
    };

    use super::*;

    fn read_request(stream: &UnixStream) -> serde_json::Value {
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn reply(stream: &mut UnixStream, request: &serde_json::Value, result: serde_json::Value) {
        writeln!(
            stream,
            "{}",
            serde_json::json!({ "id": request["id"], "result": result })
        )
        .unwrap();
    }

    fn snapshot(agents: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "type": "session_snapshot",
            "snapshot": {
                "version": "0.8.0",
                "protocol": MIN_HERDR_PROTOCOL,
                "workspaces": [],
                "tabs": [],
                "panes": [{
                    "pane_id": "w1:p1",
                    "terminal_id": "term1",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": true,
                    "agent_status": "idle",
                    "revision": 1
                }],
                "layouts": [],
                "agents": agents
            }
        })
    }

    #[test]
    fn discovery_keeps_running_sessions_sorted_with_exact_socket_paths() {
        let sessions = parse_sessions(
            r#"{"sessions":[{"name":"werk","running":true,"socket_path":"/tmp/named.sock"},{"name":"old","running":false,"socket_path":"/tmp/old.sock"},{"name":"default","running":true,"socket_path":"/tmp/default.sock"}]}"#,
        )
        .unwrap();
        assert_eq!(
            sessions,
            [
                Session {
                    name: "default".into(),
                    socket_path: "/tmp/default.sock".into()
                },
                Session {
                    name: "werk".into(),
                    socket_path: "/tmp/named.sock".into()
                },
            ]
        );
    }

    #[test]
    fn command_runner_reports_stderr_and_reaps_timeouts() {
        let error = run_command_with_timeout(
            Command::new("/bin/sh").args(["-c", "printf failure >&2; exit 7"]),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("failure"));

        let started = Instant::now();
        let error = run_command_with_timeout(
            Command::new("/bin/sh").args(["-c", "exec /bin/sleep 5"]),
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));

        let started = Instant::now();
        let error = run_command_with_timeout(
            Command::new("/bin/sh").args(["-c", "while :; do printf x; done"]),
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));

        let marker = std::env::temp_dir().join(format!(
            "herdr-micro-command-descendant-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&marker);
        run_command_with_timeout(
            Command::new("/bin/sh")
                .args(["-c", "(sleep 0.2; : > \"$HERDR_TEST_MARKER\") &"])
                .env("HERDR_TEST_MARKER", &marker),
            Duration::from_secs(1),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists(), "background descendant survived");

        let noisy = format!(
            "/usr/bin/yes x | /usr/bin/head -c {}",
            COMMAND_OUTPUT_LIMIT * 2
        );
        let output = run_command_with_timeout(
            Command::new("/bin/sh").args(["-c", &noisy]),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(output.len(), COMMAND_OUTPUT_LIMIT);
    }

    #[test]
    fn polling_publishes_only_changed_agent_snapshots() {
        let path = std::env::temp_dir().join(format!(
            "herdr-micro-follow-session-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let server_active = Arc::clone(&active);
        let refresh = Arc::new(AtomicBool::new(false));
        let server_refresh = Arc::clone(&refresh);
        let server = thread::spawn(move || {
            let (mut before, _) = listener.accept().unwrap();
            let request = read_request(&before);
            assert_eq!(request["method"], "session.snapshot");
            reply(&mut before, &request, snapshot(serde_json::json!([])));

            let (mut after_event, _) = listener.accept().unwrap();
            let request = read_request(&after_event);
            assert_eq!(request["method"], "session.snapshot");
            let mut second = snapshot(serde_json::json!([{
                "terminal_id": "term1",
                "agent": "codex",
                "agent_status": "working",
                "workspace_id": "w1",
                "tab_id": "w1:t1",
                "pane_id": "w1:p1",
                "focused": true,
                "revision": 2
            }]));
            second["snapshot"]["client_focused"] = serde_json::json!(true);
            reply(&mut after_event, &request, second.clone());

            let (mut refreshed, _) = listener.accept().unwrap();
            // A refresh request resends an unchanged snapshot.
            server_refresh.store(true, Ordering::Release);
            let request = read_request(&refreshed);
            reply(&mut refreshed, &request, second);
            server_active.store(false, Ordering::Release);
        });

        let (updates, received) = mpsc::channel();
        let stopping = AtomicBool::new(false);
        let mut connected = false;
        let session = Session {
            name: "default".into(),
            socket_path: path.clone(),
        };
        follow_session(
            &session,
            7,
            &updates,
            &active,
            &refresh,
            &stopping,
            &mut connected,
        )
        .unwrap();

        let agent_counts: Vec<_> = received
            .try_iter()
            .map(|update| match update {
                SessionUpdate::Agents {
                    agents,
                    client_focused,
                    ..
                } => (agents.len(), client_focused),
                SessionUpdate::Unavailable { .. } => unreachable!(),
            })
            .collect();
        assert!(connected);
        assert_eq!(agent_counts, [(0, None), (1, Some(true)), (1, Some(true))]);
        server.join().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn snapshots_require_nonempty_routing_identities() {
        let snapshot = |protocol: u32, pane_id: &str, terminal_id: &str| {
            serde_json::from_value::<SessionSnapshot>(serde_json::json!({
                "version": "0.8.0",
                "protocol": protocol,
                "workspaces": [],
                "tabs": [],
                "panes": [{
                    "pane_id": pane_id,
                    "terminal_id": terminal_id,
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": true,
                    "agent_status": "idle",
                    "revision": 1
                }],
                "layouts": [],
                "agents": []
            }))
            .unwrap()
        };
        assert!(validate_snapshot(snapshot(MIN_HERDR_PROTOCOL, "w1:p1", "term1")).is_ok());
        assert!(validate_snapshot(snapshot(MIN_HERDR_PROTOCOL + 1, "w1:p1", "term1")).is_ok());
        assert!(validate_snapshot(snapshot(MIN_HERDR_PROTOCOL - 1, "w1:p1", "term1")).is_err());
        assert!(validate_snapshot(snapshot(MIN_HERDR_PROTOCOL, "", "term1")).is_err());
        assert!(validate_snapshot(snapshot(MIN_HERDR_PROTOCOL, "w1:p1", "")).is_err());
    }
}
