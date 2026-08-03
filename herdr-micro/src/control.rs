//! The small local control socket shared by the Micro action scripts and daemon.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::env;
use std::fs::{self, Permissions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Receiver,
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::PLUGIN_ID;

const SOCKET_NAME: &str = "micro.sock";
const LOG_NAME: &str = "micro.log";
const MAX_LINE_BYTES: usize = 64 * 1024;

/// The plugin's persistent state directory.
pub fn state_dir() -> PathBuf {
    env::var_os("HERDR_PLUGIN_STATE_DIR").map_or_else(
        || {
            let home = env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home)
                .join(".local/state/herdr/plugins")
                .join(PLUGIN_ID)
        },
        PathBuf::from,
    )
}

pub fn control_socket() -> PathBuf {
    state_dir().join(SOCKET_NAME)
}

pub fn log_file() -> PathBuf {
    state_dir().join(LOG_NAME)
}

pub fn ensure_state_dir() -> Result<PathBuf> {
    let dir = state_dir();
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
    request_at(&control_socket(), Command::Status, timeout)
}

pub fn request_stop(timeout: Duration) -> Result<Value> {
    request_at(&control_socket(), Command::Stop, timeout)
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

/// Return a running daemon's status, otherwise launch it and poll until ready.
pub fn start_daemon<F>(launch: F, ready_timeout: Duration) -> Result<Value>
where
    F: FnOnce() -> Result<()>,
{
    start_daemon_at(&control_socket(), launch, ready_timeout)
}

pub fn start_daemon_at<F>(path: &Path, launch: F, ready_timeout: Duration) -> Result<Value>
where
    F: FnOnce() -> Result<()>,
{
    if let Ok(status) = request_at(path, Command::Status, Duration::from_millis(500)) {
        return Ok(status);
    }

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("control socket has no parent"))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    launch()?;
    let deadline = Instant::now() + ready_timeout;
    loop {
        if let Ok(status) = request_at(path, Command::Status, Duration::from_millis(250)) {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            bail!("Micro bridge did not start; see {}", log_file().display());
        }
        thread::sleep(Duration::from_millis(100));
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
    listener: Option<UnixListener>,
    path: PathBuf,
    owned: SocketIdentity,
    status: Arc<dyn Fn() -> Value + Send + Sync>,
    stop: Arc<dyn Fn() + Send + Sync>,
    stopping: Arc<AtomicBool>,
}

impl ControlServer {
    /// The synchronous daemon-loop variant for callers that already have a
    /// shutdown channel. Disconnecting the channel also ends the loop.
    pub fn run_with_shutdown(&self, shutdown: &Receiver<()>) -> Result<()> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| anyhow!("Micro bridge server is closed"))?;
        listener.set_nonblocking(true)?;
        while !self.stopping.load(Ordering::Acquire) {
            if !matches!(
                shutdown.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ) {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => self.serve_connection(stream),
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
        let stop = Arc::clone(&self.stop);
        let stopping = Arc::clone(&self.stopping);
        thread::spawn(move || {
            let _ = handle_connection(stream, status, stop, stopping);
        });
    }

    /// Remove the socket only when this server still owns the same filesystem
    /// entry. It is also performed automatically on drop.
    pub fn close(&mut self) -> Result<()> {
        drop(self.listener.take());
        if identity(&self.path)? == Some(self.owned) {
            fs::remove_file(&self.path)
                .with_context(|| format!("remove {}", self.path.display()))?;
        }
        Ok(())
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Bind the control socket, taking over only a stale socket with unchanged
/// device/inode identity. Any responsive daemon counts as live.
pub fn listen_for_control<F, S>(get_status: F, stop: S) -> Result<ControlServer>
where
    F: Fn() -> Value + Send + Sync + 'static,
    S: Fn() + Send + Sync + 'static,
{
    listen_for_control_at(control_socket(), get_status, stop)
}

pub fn listen_for_control_at<F, S>(path: PathBuf, get_status: F, stop: S) -> Result<ControlServer>
where
    F: Fn() -> Value + Send + Sync + 'static,
    S: Fn() + Send + Sync + 'static,
{
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("control socket has no parent"))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;

    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) if error.raw_os_error() == Some(libc::EADDRINUSE) => {
            let before = identity(&path)?;
            if request_at(&path, Command::Status, Duration::from_millis(500)).is_ok() {
                bail!("Micro bridge is already running");
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
        listener: Some(listener),
        path,
        owned,
        status: Arc::new(get_status),
        stop: Arc::new(stop),
        stopping: Arc::new(AtomicBool::new(false)),
    })
}

fn handle_connection(
    mut stream: UnixStream,
    status: Arc<dyn Fn() -> Value + Send + Sync>,
    stop: Arc<dyn Fn() + Send + Sync>,
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
    let response = match (&parsed, command) {
        (Err(_) | Ok(Value::Null), _) => json!({ "error": "invalid request" }),
        (_, Some(Command::Status)) => status(),
        (_, Some(Command::Stop)) => json!({ "stopping": true }),
        _ => json!({ "error": "unknown command" }),
    };
    stream.write_all(response.to_string().as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    if command == Some(Command::Stop) && !stopping.swap(true, Ordering::AcqRel) {
        stop();
    }
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    fn temp_dir(name: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = env::temp_dir().join(format!(
            "herdr-micro-control-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn start_test_server(
        path: PathBuf,
        stops: Arc<AtomicUsize>,
    ) -> (Arc<ControlServer>, mpsc::Sender<()>, thread::JoinHandle<()>) {
        let server = Arc::new(
            listen_for_control_at(
                path,
                || json!({ "running": true }),
                move || {
                    stops.fetch_add(1, Ordering::Relaxed);
                },
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
        let (server, _shutdown, runner) =
            start_test_server(path.clone(), Arc::new(AtomicUsize::new(0)));
        assert_eq!(
            request_at(&path, Command::Status, Duration::from_secs(1)).unwrap(),
            json!({ "running": true })
        );
        request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap();
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
        let stops = Arc::new(AtomicUsize::new(0));
        let (server, _shutdown, runner) = start_test_server(path.clone(), stops);
        let mut client = UnixStream::connect(&path).unwrap();
        client.write_all(b"{\"command\":\"status\"}\n").unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&read_line(&mut client).unwrap()).unwrap(),
            json!({ "running": true })
        );
        request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap();
        drop(server);
        runner.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_and_unknown_requests_return_errors() {
        let dir = temp_dir("errors");
        let path = dir.join(SOCKET_NAME);
        let (server, _shutdown, runner) =
            start_test_server(path.clone(), Arc::new(AtomicUsize::new(0)));
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
        drop(server);
        runner.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn duplicate_start_detects_live_daemon() {
        let dir = temp_dir("duplicate");
        let path = dir.join(SOCKET_NAME);
        let (server, _shutdown, runner) =
            start_test_server(path.clone(), Arc::new(AtomicUsize::new(0)));
        let error = match listen_for_control_at(path.clone(), || json!({}), || {}) {
            Ok(_) => panic!("duplicate bind succeeded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("already running"));
        request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap();
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
        let mut server = listen_for_control_at(path.clone(), || json!({}), || {}).unwrap();
        assert_ne!(identity(&path).unwrap().unwrap(), stale);
        server.close().unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn socket_is_owner_only_and_stop_is_idempotent() {
        let dir = temp_dir("stop");
        let path = dir.join(SOCKET_NAME);
        let stops = Arc::new(AtomicUsize::new(0));
        let (server, _shutdown, runner) = start_test_server(path.clone(), Arc::clone(&stops));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap(),
            json!({ "stopping": true })
        );
        runner.join().unwrap();
        assert_eq!(stops.load(Ordering::Relaxed), 1);
        drop(server);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn start_waits_for_readiness() {
        let dir = temp_dir("start");
        let path = dir.join(SOCKET_NAME);
        let (ready_tx, ready_rx) = mpsc::channel();
        let launch_path = path.clone();
        let result = start_daemon_at(
            &path,
            move || {
                let (server, shutdown, runner) =
                    start_test_server(launch_path, Arc::new(AtomicUsize::new(0)));
                ready_tx.send((server, shutdown, runner)).unwrap();
                Ok(())
            },
            Duration::from_secs(1),
        );
        assert_eq!(result.unwrap(), json!({ "running": true }));
        let (server, _shutdown, runner) = ready_rx.recv().unwrap();
        request_at(&path, Command::Stop, Duration::from_secs(1)).unwrap();
        drop(server);
        runner.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
