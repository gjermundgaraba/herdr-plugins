//! Herdr session discovery and direct socket access.

use anyhow::{Context, Result, bail};
use herdr_client::{AgentInfo, Client, SessionSnapshot};
use serde::Deserialize;
use std::{
    env,
    io::Read,
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
const MIN_HERDR_PROTOCOL: u32 = 19;
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
    thread: thread::Thread,
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
    let worker = thread::spawn(move || {
        let mut retry = Duration::from_millis(250);
        while worker_active.load(Ordering::Acquire) && !stopping.load(Ordering::Acquire) {
            let client = session.client();
            let mut connected = false;
            let result = follow_session(
                &client,
                &session.name,
                generation,
                &updates,
                &worker_active,
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
        thread,
    }
}

fn follow_session(
    client: &Client,
    session: &str,
    generation: u64,
    updates: &Sender<SessionUpdate>,
    active: &AtomicBool,
    stopping: &AtomicBool,
    connected: &mut bool,
) -> Result<()> {
    if !active.load(Ordering::Acquire) || stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut previous = None;
    loop {
        let agents = current_snapshot(client)?.agents;
        if previous.as_ref() != Some(&agents) {
            send_agents(session, generation, updates, agents.clone())?;
            previous = Some(agents);
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
) -> Result<()> {
    updates
        .send(SessionUpdate::Agents {
            session: session.into(),
            generation,
            agents,
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
        .env_remove("HERDR_WORKSPACE_ID")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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

fn run_command_with_timeout(command: &mut Command, timeout: Duration) -> Result<String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run {program}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = std::io::BufReader::new(stdout).read_to_end(&mut bytes);
        bytes
    });
    let err_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = std::io::BufReader::new(stderr).read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().context("wait for Herdr session list")? {
            break (status, false);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            break (
                child.wait().context("reap timed out Herdr session list")?,
                true,
            );
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
    if timed_out {
        bail!(
            "Herdr session discovery timed out after {}s",
            timeout.as_secs_f64()
        );
    }
    if !status.success() {
        bail!("Herdr session discovery failed: {detail}");
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
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
    fn polling_publishes_only_changed_agent_snapshots() {
        let path = std::env::temp_dir().join(format!(
            "herdr-micro-follow-session-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let server_active = Arc::clone(&active);
        let server = thread::spawn(move || {
            let (mut before, _) = listener.accept().unwrap();
            let request = read_request(&before);
            assert_eq!(request["method"], "session.snapshot");
            reply(&mut before, &request, snapshot(serde_json::json!([])));

            let (mut after_event, _) = listener.accept().unwrap();
            let request = read_request(&after_event);
            assert_eq!(request["method"], "session.snapshot");
            reply(
                &mut after_event,
                &request,
                snapshot(serde_json::json!([{
                    "terminal_id": "term1",
                    "agent": "codex",
                    "agent_status": "working",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "pane_id": "w1:p1",
                    "focused": true,
                    "revision": 2
                }])),
            );
            server_active.store(false, Ordering::Release);
        });

        let (updates, received) = mpsc::channel();
        let stopping = AtomicBool::new(false);
        let mut connected = false;
        follow_session(
            &Client::new(&path).with_timeout(Duration::from_secs(1)),
            "default",
            7,
            &updates,
            &active,
            &stopping,
            &mut connected,
        )
        .unwrap();

        let agent_counts: Vec<_> = received
            .try_iter()
            .map(|update| match update {
                SessionUpdate::Agents { agents, .. } => agents.len(),
                SessionUpdate::Unavailable { .. } => unreachable!(),
            })
            .collect();
        assert!(connected);
        assert_eq!(agent_counts, [0, 1]);
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
