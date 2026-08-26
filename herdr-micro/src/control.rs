//! The small local control socket shared by the Micro action scripts and daemon.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::fs::{self, Permissions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::Receiver,
};
use std::thread;
use std::time::{Duration, Instant};

const SOCKET_NAME: &str = "micro.sock";
const LOG_NAME: &str = "micro.log";
const MAX_LINE_BYTES: usize = 64 * 1024;

pub fn control_socket() -> Result<PathBuf> {
    Ok(crate::plugin_paths()?.run_dir().join(SOCKET_NAME))
}

pub fn log_file() -> Result<PathBuf> {
    Ok(crate::plugin_paths()?.logs_dir().join(LOG_NAME))
}

pub fn backup_dir() -> Result<PathBuf> {
    let dir = crate::plugin_paths()?.data_dir().join("backups");
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Status,
    Stop,
}

impl Command {
    fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Stop => "stop",
        }
    }
}

pub fn request_status(timeout: Duration) -> Result<Value> {
    request_at(&control_socket()?, Command::Status, timeout)
}

pub fn request_stop(timeout: Duration) -> Result<Value> {
    request_at(&control_socket()?, Command::Stop, timeout)
}

/// Send exactly one newline-delimited JSON request and read exactly one response.
pub fn request_at(path: &Path, command: Command, timeout: Duration) -> Result<Value> {
    let started = Instant::now();
    let stream =
        UnixStream::connect(path).with_context(|| format!("connect {}", path.display()))?;
    let remaining = timeout
        .checked_sub(started.elapsed())
        .ok_or_else(|| anyhow!("Micro bridge timed out"))?;
    stream.set_read_timeout(Some(remaining))?;
    stream.set_write_timeout(Some(remaining))?;
    let mut stream = stream;
    let request = format!("{}\n", json!({ "command": command.name() }));
    stream
        .write_all(request.as_bytes())
        .context("write Micro bridge request")?;
    stream.flush().context("flush Micro bridge request")?;

    let line = read_line(&mut stream).context("read Micro bridge response")?;
    serde_json::from_slice(&line).context("parse Micro bridge response")
}

/// Return a compatible daemon when one is live; otherwise stop it before
/// starting this executable.
pub fn start_daemon_versioned<F>(
    version: &str,
    protocol: u32,
    launch: F,
    ready_timeout: Duration,
) -> Result<Value>
where
    F: FnOnce() -> Result<()>,
{
    let path = control_socket()?;
    let log = log_file()?;
    start_daemon_versioned_at(&path, &log, version, protocol, launch, ready_timeout)
}

fn start_daemon_versioned_at<F>(
    path: &Path,
    log: &Path,
    version: &str,
    protocol: u32,
    launch: F,
    ready_timeout: Duration,
) -> Result<Value>
where
    F: FnOnce() -> Result<()>,
{
    let old_socket = identity(path)?;
    if let Ok(status) = request_at(path, Command::Status, Duration::from_millis(500)) {
        if status.get("version").and_then(Value::as_str) == Some(version)
            && status.get("protocol").and_then(Value::as_u64) == Some(u64::from(protocol))
        {
            return Ok(status);
        }
        // An already-stopping daemon answers with an error payload; only a
        // live incompatible one still needs the stop request.
        if status.get("error").is_none() {
            request_at(path, Command::Stop, Duration::from_secs(2))
                .context("stop incompatible Micro bridge")?;
        }
        let deadline = Instant::now() + ready_timeout;
        while identity(path)? == old_socket {
            if Instant::now() >= deadline {
                bail!("previous Micro bridge did not stop; see {}", log.display());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    launch()?;
    let deadline = Instant::now() + ready_timeout;
    loop {
        if let Ok(status) = request_at(path, Command::Status, Duration::from_millis(250))
            && status.get("version").and_then(Value::as_str) == Some(version)
            && status.get("protocol").and_then(Value::as_u64) == Some(u64::from(protocol))
        {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            bail!("Micro bridge did not start; see {}", log.display());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketIdentity {
    dev: u64,
    ino: u64,
}

fn identity(path: &Path) -> Result<Option<SocketIdentity>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(SocketIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        })),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("stat {}", path.display())),
    }
}

/// A bound server. Every connection is served on its own short-lived thread so
/// status calls overlap.
pub struct ControlServer {
    listener: UnixListener,
    path: PathBuf,
    owned: SocketIdentity,
    status: Arc<Mutex<Value>>,
    stopping: Arc<AtomicBool>,
}

impl ControlServer {
    /// The synchronous daemon-loop variant for callers that already have a
    /// shutdown channel. Disconnecting the channel also ends the loop.
    pub fn run_with_shutdown(&self, shutdown: &Receiver<()>) -> Result<()> {
        self.listener.set_nonblocking(true)?;
        loop {
            if !matches!(
                shutdown.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ) {
                break;
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // Accepted fds inherit O_NONBLOCK from the listener;
                    // reads must block for set_read_timeout to apply.
                    let _ = stream.set_nonblocking(false);
                    self.serve_connection(stream);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error).context("accept Micro bridge connection"),
            }
        }
        Ok(())
    }

    fn serve_connection(&self, stream: UnixStream) {
        let status = Arc::clone(&self.status);
        let stopping = Arc::clone(&self.stopping);
        thread::spawn(move || {
            let _ = handle_connection(stream, status, stopping);
        });
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        if identity(&self.path).ok().flatten() == Some(self.owned) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Bind the control socket, taking over only a stale socket with unchanged
/// device/inode identity. Any daemon answering status without an error
/// payload counts as live.
///
/// `stopping` is shared with the daemon: any shutdown path (stop command,
/// signal, idle release) sets it, and the server then refuses status so a
/// racing `start` never mistakes a draining daemon for a live one.
pub fn listen_for_control(
    status: Arc<Mutex<Value>>,
    stopping: Arc<AtomicBool>,
) -> Result<ControlServer> {
    listen_for_control_at(control_socket()?, status, stopping)
}

pub fn listen_for_control_at(
    path: PathBuf,
    status: Arc<Mutex<Value>>,
    stopping: Arc<AtomicBool>,
) -> Result<ControlServer> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("control socket has no parent"))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;

    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) if error.raw_os_error() == Some(libc::EADDRINUSE) => {
            let before = identity(&path)?;
            match UnixStream::connect(&path) {
                Ok(_) => bail!("Micro bridge socket is already active"),
                Err(error) if error.raw_os_error() == Some(libc::ECONNREFUSED) => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("connect {}", path.display()));
                }
            }
            if before.is_none() || identity(&path)? != before {
                bail!("Micro bridge socket changed during startup");
            }
            fs::remove_file(&path).with_context(|| format!("remove stale {}", path.display()))?;
            UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?
        }
        Err(error) => return Err(error).with_context(|| format!("bind {}", path.display())),
    };

    fs::set_permissions(&path, Permissions::from_mode(0o600))
        .with_context(|| format!("chmod {}", path.display()))?;
    let owned = identity(&path)?.ok_or_else(|| anyhow!("control socket disappeared after bind"))?;
    Ok(ControlServer {
        listener,
        path,
        owned,
        status,
        stopping,
    })
}

fn handle_connection(
    mut stream: UnixStream,
    status: Arc<Mutex<Value>>,
    stopping: Arc<AtomicBool>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let request = read_line(&mut stream)?;
    let parsed = serde_json::from_slice::<Value>(&request);
    let command = parsed
        .as_ref()
        .ok()
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .and_then(|name| match name {
            "status" => Some(Command::Status),
            "stop" => Some(Command::Stop),
            _ => None,
        });
    // Flag the shutdown before replying so no status answered after the stop
    // acknowledgement can pass for a live daemon.
    if command == Some(Command::Stop) {
        stopping.store(true, Ordering::Release);
    }
    let response = match (&parsed, command) {
        (Err(_) | Ok(Value::Null), _) => json!({ "error": "invalid request" }),
        // A stopping daemon must not answer status: a racing `start` would
        // mistake it for a live compatible daemon and skip launching.
        (_, Some(Command::Status)) if stopping.load(Ordering::Acquire) => {
            json!({ "error": "Micro bridge is stopping" })
        }
        (_, Some(Command::Status)) => status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
        (_, Some(Command::Stop)) => json!({ "stopping": true }),
        _ => json!({ "error": "unknown command" }),
    };
    stream.write_all(response.to_string().as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn read_line(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => bail!("connection closed before newline"),
            Ok(_) if byte[0] == b'\n' => return Ok(line),
            Ok(_) if line.len() < MAX_LINE_BYTES => line.push(byte[0]),
            Ok(_) => bail!("request exceeds {MAX_LINE_BYTES} bytes"),
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::{
        env,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn temp_dir(name: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // Short prefix: socket paths must stay under the 104-byte SUN_LEN.
        let dir = env::temp_dir().join(format!(
            "hm-ctl-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn start_test_server(
        path: PathBuf,
        stopping: Arc<AtomicBool>,
    ) -> (Arc<ControlServer>, mpsc::Sender<()>, thread::JoinHandle<()>) {
        let server = Arc::new(
            listen_for_control_at(
                path,
                Arc::new(Mutex::new(json!({ "running": true }))),
                stopping,
            )
            .unwrap(),
        );
        let (shutdown, shutdown_rx) = mpsc::channel();
        let runner = Arc::clone(&server);
        let thread = thread::spawn(move || runner.run_with_shutdown(&shutdown_rx).unwrap());
        (server, shutdown, thread)
    }

    #[test]
    fn rust_client_and_server() {
        let dir = temp_dir("rust");
        let path = dir.join(SOCKET_NAME);
        let (server, shutdown, runner) =
            start_test_server(path.clone(), Arc::new(AtomicBool::new(false)));
        assert_eq!(
            request_at(&path, Command::Status, Duration::from_secs(1)).unwrap(),
            json!({ "running": true })
        );
        request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap();
        shutdown.send(()).unwrap();
        drop(server);
        runner.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn client_accepts_newline_json_server() {
        let dir = temp_dir("protocol-server");
        let path = dir.join(SOCKET_NAME);
        let listener = UnixListener::bind(&path).unwrap();
        let thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            assert_eq!(read_line(&mut stream).unwrap(), br#"{"command":"status"}"#);
            stream.write_all(b"{\"fixture\":true}\n").unwrap();
        });
        assert_eq!(
            request_at(&path, Command::Status, Duration::from_secs(1)).unwrap(),
            json!({ "fixture": true })
        );
        thread.join().unwrap();
        fs::remove_file(&path).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn newline_json_client_reaches_server() {
        let dir = temp_dir("protocol-client");
        let path = dir.join(SOCKET_NAME);
        let (server, shutdown, runner) =
            start_test_server(path.clone(), Arc::new(AtomicBool::new(false)));
        let mut client = UnixStream::connect(&path).unwrap();
        client.write_all(b"{\"command\":\"status\"}\n").unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&read_line(&mut client).unwrap()).unwrap(),
            json!({ "running": true })
        );
        request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap();
        shutdown.send(()).unwrap();
        drop(server);
        runner.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_and_unknown_requests_return_errors() {
        let dir = temp_dir("errors");
        let path = dir.join(SOCKET_NAME);
        let (server, shutdown, runner) =
            start_test_server(path.clone(), Arc::new(AtomicBool::new(false)));
        for (request, expected) in [
            (
                b"not json\n".as_slice(),
                json!({ "error": "invalid request" }),
            ),
            (
                b"{\"command\":\"other\"}\n".as_slice(),
                json!({ "error": "unknown command" }),
            ),
        ] {
            let mut client = UnixStream::connect(&path).unwrap();
            client.write_all(request).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&read_line(&mut client).unwrap()).unwrap(),
                expected
            );
        }
        request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap();
        shutdown.send(()).unwrap();
        drop(server);
        runner.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn duplicate_start_detects_live_daemon() {
        let dir = temp_dir("duplicate");
        let path = dir.join(SOCKET_NAME);
        let (server, shutdown, runner) =
            start_test_server(path.clone(), Arc::new(AtomicBool::new(false)));
        let error = match listen_for_control_at(
            path.clone(),
            Arc::new(Mutex::new(json!({}))),
            Arc::new(AtomicBool::new(false)),
        ) {
            Ok(_) => panic!("duplicate bind succeeded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("already active"));
        request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap();
        shutdown.send(()).unwrap();
        drop(server);
        runner.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_socket_is_replaced_without_touching_a_new_one() {
        let dir = temp_dir("stale");
        let path = dir.join(SOCKET_NAME);
        drop(UnixListener::bind(&path).unwrap());
        let stale = identity(&path).unwrap().unwrap();
        let server = listen_for_control_at(
            path.clone(),
            Arc::new(Mutex::new(json!({}))),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_ne!(identity(&path).unwrap().unwrap(), stale);
        drop(server);
        assert!(!path.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn socket_is_owner_only_and_stop_sets_shared_flag() {
        let dir = temp_dir("stop");
        let path = dir.join(SOCKET_NAME);
        let stopping = Arc::new(AtomicBool::new(false));
        let (server, shutdown, runner) = start_test_server(path.clone(), Arc::clone(&stopping));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap(),
            json!({ "stopping": true })
        );
        assert!(stopping.load(Ordering::Acquire));
        shutdown.send(()).unwrap();
        runner.join().unwrap();
        drop(server);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stopping_server_refuses_status() {
        let dir = temp_dir("stopping-status");
        let path = dir.join(SOCKET_NAME);
        let (server, shutdown, runner) =
            start_test_server(path.clone(), Arc::new(AtomicBool::new(false)));
        // The flag is set before the stop response is written, so the next
        // status is deterministically refused.
        request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap();
        assert_eq!(
            request_at(&path, Command::Status, Duration::from_secs(1)).unwrap(),
            json!({ "error": "Micro bridge is stopping" })
        );
        shutdown.send(()).unwrap();
        drop(server);
        runner.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn daemon_side_flag_refuses_status() {
        let dir = temp_dir("daemon-stopping");
        let path = dir.join(SOCKET_NAME);
        let stopping = Arc::new(AtomicBool::new(false));
        let (server, shutdown, runner) = start_test_server(path.clone(), Arc::clone(&stopping));
        // A signal or idle release sets the daemon's flag without any stop
        // command; the server must still refuse status.
        stopping.store(true, Ordering::Release);
        assert_eq!(
            request_at(&path, Command::Status, Duration::from_secs(1)).unwrap(),
            json!({ "error": "Micro bridge is stopping" })
        );
        shutdown.send(()).unwrap();
        drop(server);
        runner.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stopping_daemon_is_replaced_without_stop_request() {
        let dir = temp_dir("stopping-start");
        let path = dir.join(SOCKET_NAME);
        let old_listener = UnixListener::bind(&path).unwrap();
        let old_path = path.clone();
        let old_runner = thread::spawn(move || {
            // Answer exactly one status request, then finish the teardown.
            // A stop request would hit the removed socket and fail the start.
            let (mut stream, _) = old_listener.accept().unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&read_line(&mut stream).unwrap()).unwrap(),
                json!({"command":"status"})
            );
            writeln!(stream, "{}", json!({"error":"Micro bridge is stopping"})).unwrap();
            drop(old_listener);
            fs::remove_file(old_path).unwrap();
        });
        let (ready_tx, ready_rx) = mpsc::channel();
        let launch_path = path.clone();
        let status = start_daemon_versioned_at(
            &path,
            &dir.join("micro.log"),
            "0.9.0",
            1,
            move || {
                let listener = UnixListener::bind(&launch_path).unwrap();
                let runner = thread::spawn(move || {
                    let (mut stream, _) = listener.accept().unwrap();
                    read_line(&mut stream).unwrap();
                    writeln!(stream, "{}", json!({"version":"0.9.0", "protocol":1})).unwrap();
                });
                ready_tx.send(runner).unwrap();
                Ok(())
            },
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(status["version"], "0.9.0");
        ready_rx.recv().unwrap().join().unwrap();
        old_runner.join().unwrap();
        fs::remove_file(&path).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn version_mismatch_stops_and_relaunches() {
        let dir = temp_dir("versioned-start");
        let path = dir.join(SOCKET_NAME);
        let old_listener = UnixListener::bind(&path).unwrap();
        let old_path = path.clone();
        let old_runner = thread::spawn(move || {
            for (command, response) in [
                (Command::Status, json!({"version":"0.8.0", "protocol":1})),
                (Command::Stop, json!({"stopping":true})),
            ] {
                let (mut stream, _) = old_listener.accept().unwrap();
                assert_eq!(
                    serde_json::from_slice::<Value>(&read_line(&mut stream).unwrap()).unwrap(),
                    json!({"command":command.name()})
                );
                writeln!(stream, "{response}").unwrap();
            }
            drop(old_listener);
            fs::remove_file(old_path).unwrap();
        });
        let (ready_tx, ready_rx) = mpsc::channel();
        let launched = Arc::new(AtomicBool::new(false));
        let launch_path = path.clone();
        let launched_by_start = Arc::clone(&launched);
        let status = start_daemon_versioned_at(
            &path,
            &dir.join("micro.log"),
            "0.9.0",
            1,
            move || {
                launched_by_start.store(true, Ordering::Relaxed);
                let listener = UnixListener::bind(&launch_path).unwrap();
                let runner = thread::spawn(move || {
                    let (mut stream, _) = listener.accept().unwrap();
                    assert_eq!(
                        serde_json::from_slice::<Value>(&read_line(&mut stream).unwrap()).unwrap(),
                        json!({"command":"status"})
                    );
                    writeln!(stream, "{}", json!({"version":"0.9.0", "protocol":1})).unwrap();
                });
                ready_tx.send(runner).unwrap();
                Ok(())
            },
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(launched.load(Ordering::Relaxed));
        assert_eq!(status["version"], "0.9.0");
        ready_rx.recv().unwrap().join().unwrap();
        old_runner.join().unwrap();
        fs::remove_file(&path).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
