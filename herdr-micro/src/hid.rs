//! Privileged Codex Micro access over a local, versioned NDJSON socket.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    ffi::{CString, c_char, c_int, c_void},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        io::{AsRawFd, FromRawFd},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Sender, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    actions::layer_identity,
    device::{DEFAULT_REQUEST_TIMEOUT, DeviceEvent, MicroDevice},
};

pub const HID_PROTOCOL_VERSION: u32 = 1;
pub const HID_LAUNCH_SOCKET_NAME: &str = "Control";
pub const HID_MAX_LINE_BYTES: usize = 64 * 1024;
pub const HELPER_LABEL: &str = "dev.herdr.herdr-micro-hid";
// Increment whenever privileged helper code changes.
pub const HELPER_VERSION: &str = "14";
const HID_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const HELPER_SHUTDOWN_TIMEOUT: Duration =
    MicroDevice::NATIVE_WATCHDOG_TIMEOUT.saturating_add(Duration::from_secs(1));
static HELPER_SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn request_helper_shutdown(_signal: c_int) {
    HELPER_SHUTDOWN.store(true, Ordering::Release);
}

pub fn hid_socket_path(uid: libc::uid_t) -> PathBuf {
    PathBuf::from(format!("/var/run/{HELPER_LABEL}-{uid}.sock"))
}

pub fn current_hid_socket_path() -> PathBuf {
    // SAFETY: getuid has no preconditions.
    hid_socket_path(unsafe { libc::getuid() })
}

type Reply = std::result::Result<Value, String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HelperErrorCode {
    Helper,
    Unavailable,
}

impl HelperErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Helper => "helper",
            Self::Unavailable => "unavailable",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "helper" => Some(Self::Helper),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }

    fn device_state(self) -> &'static str {
        match self {
            Self::Helper => "helper-error",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug)]
struct HelperConnectionError {
    code: HelperErrorCode,
    message: String,
}

impl std::fmt::Display for HelperConnectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HelperConnectionError {}

pub(crate) fn connection_error_state(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<HelperConnectionError>()
        .map_or("helper-error", |error| error.code.device_state())
}

pub struct HidClient {
    writer: Arc<Mutex<UnixStream>>,
    pending: Arc<Mutex<HashMap<u64, SyncSender<Reply>>>>,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    event_tx: Sender<DeviceEvent>,
    reader: Option<JoinHandle<()>>,
}

impl HidClient {
    pub fn connect(event_tx: Sender<DeviceEvent>) -> Result<Self> {
        Self::connect_at(&current_hid_socket_path(), event_tx, 0)
    }

    fn connect_at(
        path: &Path,
        event_tx: Sender<DeviceEvent>,
        expected_server_uid: libc::uid_t,
    ) -> Result<Self> {
        let mut stream = UnixStream::connect(path)
            .with_context(|| format!("connect privileged USB helper at {}", path.display()))?;
        let peer = peer_euid(&stream)?;
        if peer != expected_server_uid {
            bail!("privileged USB helper has unexpected uid {peer}")
        }
        stream.set_read_timeout(Some(HID_CONNECT_TIMEOUT))?;
        stream.set_write_timeout(Some(HID_CONNECT_TIMEOUT))?;
        write_message(
            &mut stream,
            &json!({
                "v": HID_PROTOCOL_VERSION,
                "type": "hello",
                "helperVersion": HELPER_VERSION,
            }),
        )?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let hello = read_message(&mut reader).context("read USB helper handshake")?;
        validate_hello(&hello)?;
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(Some(DEFAULT_REQUEST_TIMEOUT))?;

        let writer = Arc::new(Mutex::new(stream));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let reader_pending = Arc::clone(&pending);
        let reader_closed = Arc::clone(&closed);
        let reader = thread::Builder::new()
            .name("herdr-micro-hid-ipc".into())
            .spawn({
                let event_tx = event_tx.clone();
                move || client_reader(reader, event_tx, reader_pending, reader_closed)
            })?;
        Ok(Self {
            writer,
            pending,
            next_id: AtomicU64::new(1),
            closed,
            event_tx,
            reader: Some(reader),
        })
    }

    pub fn send(&self, method: impl Into<String>, params: Option<Value>) -> Result<()> {
        self.call("send", method.into(), params).map(|_| ())
    }

    pub fn request(&self, method: impl Into<String>, params: Option<Value>) -> Result<Value> {
        self.call("request", method.into(), params)
    }

    fn call(&self, kind: &'static str, method: String, params: Option<Value>) -> Result<Value> {
        if self.closed.load(Ordering::Acquire) {
            bail!("device disconnected")
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.pending
            .lock()
            .map_err(|_| anyhow!("HID reply table poisoned"))?
            .insert(id, reply_tx);
        let message = json!({
            "v": HID_PROTOCOL_VERSION,
            "type": kind,
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(error) = self.write(&message) {
            self.remove_pending(id);
            self.disconnect(error.to_string());
            return Err(error);
        }
        let wait = DEFAULT_REQUEST_TIMEOUT
            .checked_add(Duration::from_millis(200))
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT);
        match reply_rx.recv_timeout(wait) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(anyhow!(error)),
            Err(_) => {
                self.remove_pending(id);
                bail!("request {id} timed out")
            }
        }
    }

    fn write(&self, message: &Value) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow!("HID socket writer poisoned"))?;
        write_message(&mut *writer, message)
    }

    fn remove_pending(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }

    fn disconnect(&self, error: String) {
        let notify = !self.closed.swap(true, Ordering::AcqRel);
        self.shutdown_writer();
        if notify {
            let _ = self.event_tx.send(DeviceEvent::Disconnected { error });
        }
    }

    fn shutdown_writer(&self) {
        let writer = self.writer.lock().unwrap();
        let _ = writer.shutdown(std::net::Shutdown::Both);
    }

    pub fn close(&mut self) -> Result<()> {
        let result = if !self.closed.swap(true, Ordering::AcqRel) {
            self.close_remote()
        } else {
            Ok(())
        };
        self.shutdown_writer();
        let joined = if self
            .reader
            .take()
            .map(JoinHandle::join)
            .transpose()
            .is_err()
        {
            Err(anyhow!("HID IPC reader thread panicked"))
        } else {
            Ok(())
        };
        combine_results([result, joined])
    }

    fn close_remote(&self) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.pending
            .lock()
            .map_err(|_| anyhow!("HID reply table poisoned"))?
            .insert(id, reply_tx);
        if let Err(error) = self.write(&json!({
            "v": HID_PROTOCOL_VERSION,
            "type": "close",
            "id": id,
        })) {
            self.remove_pending(id);
            return Err(error);
        }
        reply_rx
            .recv_timeout(HELPER_SHUTDOWN_TIMEOUT)
            .map_err(|_| anyhow!("USB helper close timed out"))?
            .map(|_| ())
            .map_err(anyhow::Error::msg)
    }
}

impl Drop for HidClient {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn client_reader(
    mut reader: BufReader<UnixStream>,
    event_tx: Sender<DeviceEvent>,
    pending: Arc<Mutex<HashMap<u64, SyncSender<Reply>>>>,
    closed: Arc<AtomicBool>,
) {
    let result = (|| -> Result<()> {
        loop {
            let message = read_message(&mut reader)?;
            require_version(&message)?;
            match message.get("type").and_then(Value::as_str) {
                Some("reply") => {
                    let id = message
                        .get("id")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| anyhow!("HID reply is missing an id"))?;
                    let reply = match message.get("error") {
                        Some(Value::String(error)) => Err(error.clone()),
                        Some(_) => bail!("HID reply error must be a string"),
                        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    if let Some(tx) = pending.lock().ok().and_then(|mut map| map.remove(&id)) {
                        let _ = tx.send(reply);
                    }
                }
                Some("event") => {
                    let event = serde_json::from_value(
                        message
                            .get("event")
                            .cloned()
                            .ok_or_else(|| anyhow!("HID event is missing its payload"))?,
                    )?;
                    let _ = event_tx.send(event);
                }
                Some("error") => {
                    bail!(
                        "{}",
                        message
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("USB helper protocol error")
                    )
                }
                _ => bail!("unexpected USB helper message"),
            }
        }
    })();
    let intentional = closed.swap(true, Ordering::AcqRel);
    let error = result
        .err()
        .map(|error| error.to_string())
        .unwrap_or_else(|| "device disconnected".into());
    if let Ok(mut replies) = pending.lock() {
        for (_, reply) in replies.drain() {
            let _ = reply.send(Err(error.clone()));
        }
    }
    if !intentional {
        let _ = event_tx.send(DeviceEvent::Disconnected { error });
    }
}

fn validate_hello(message: &Value) -> Result<()> {
    require_version(message)?;
    match message.get("type").and_then(Value::as_str) {
        Some("hello")
            if message.get("helperVersion").and_then(Value::as_str) == Some(HELPER_VERSION) =>
        {
            Ok(())
        }
        Some("hello") => bail!("incompatible USB helper version"),
        Some("error") => Err(HelperConnectionError {
            code: message
                .get("code")
                .and_then(Value::as_str)
                .and_then(HelperErrorCode::parse)
                .unwrap_or(HelperErrorCode::Helper),
            message: message
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("USB helper rejected the connection")
                .into(),
        }
        .into()),
        _ => bail!("invalid USB helper handshake"),
    }
}

fn require_version(message: &Value) -> Result<()> {
    if message.get("v").and_then(Value::as_u64) != Some(u64::from(HID_PROTOCOL_VERSION)) {
        bail!("incompatible USB helper protocol")
    }
    Ok(())
}

fn read_message(stream: &mut impl BufRead) -> Result<Value> {
    let line = read_line(stream)?;
    serde_json::from_slice(&line).context("parse HID IPC message")
}

fn read_line(stream: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    stream
        .take((HID_MAX_LINE_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if line.last() == Some(&b'\n') {
        line.pop();
        return Ok(line);
    }
    if line.len() > HID_MAX_LINE_BYTES {
        bail!("HID IPC message exceeds {HID_MAX_LINE_BYTES} bytes")
    }
    bail!("HID IPC connection closed")
}

fn write_message(stream: &mut impl Write, message: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(message)?;
    if bytes.len() > HID_MAX_LINE_BYTES {
        bail!("HID IPC message exceeds {HID_MAX_LINE_BYTES} bytes")
    }
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    Ok(())
}

fn peer_euid(stream: &UnixStream) -> Result<libc::uid_t> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: both output pointers are valid and the stream owns a live descriptor.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error()).context("read HID peer credentials");
    }
    Ok(uid)
}

pub fn run_hid_server(allowed_uid: libc::uid_t) -> Result<()> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        bail!("herdr-micro-hid must run as root")
    }
    if allowed_uid == 0 {
        bail!("allowed HID client uid must be non-root")
    }
    run_server(activated_listener()?, allowed_uid)
}

fn run_server(listener: UnixListener, allowed_uid: libc::uid_t) -> Result<()> {
    install_signal_handler(
        libc::SIGTERM,
        request_helper_shutdown as *const () as libc::sighandler_t,
    )
    .context("install SIGTERM handler")?;
    install_signal_handler(libc::SIGALRM, libc::SIG_DFL).context("install native HID watchdog")?;
    unblock_helper_signals()?;
    listener.set_nonblocking(true)?;
    let Some((mut stream, reader)) =
        accept_authenticated_client(&listener, allowed_uid, HID_CONNECT_TIMEOUT)?
    else {
        return Ok(());
    };
    if HELPER_SHUTDOWN.load(Ordering::Acquire) {
        return Ok(());
    }
    serve_client(&mut stream, reader)
}

fn accept_authenticated_client(
    listener: &UnixListener,
    allowed_uid: libc::uid_t,
    idle_timeout: Duration,
) -> Result<Option<(UnixStream, BufReader<UnixStream>)>> {
    let idle_deadline = Instant::now() + idle_timeout;
    loop {
        if HELPER_SHUTDOWN.load(Ordering::Acquire) || Instant::now() >= idle_deadline {
            return Ok(None);
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .context("make accepted HID client blocking")?;
                let peer = match peer_euid(&stream) {
                    Ok(peer) => peer,
                    Err(error) => {
                        let _ =
                            write_error(&mut stream, HelperErrorCode::Helper, &error.to_string());
                        continue;
                    }
                };
                if peer != allowed_uid {
                    let _ = write_error(
                        &mut stream,
                        HelperErrorCode::Helper,
                        "unauthorized HID client",
                    );
                    continue;
                }
                let reader = match authenticate_client(&mut stream) {
                    Ok(reader) => reader,
                    Err(error) => {
                        eprintln!("herdr-micro-hid client: {error:#}");
                        continue;
                    }
                };
                return Ok(Some((stream, reader)));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("accept HID client"),
        }
    }
}

fn install_signal_handler(signal: c_int, handler: libc::sighandler_t) -> Result<()> {
    // SAFETY: zero is a valid base for sigaction, the mask is initialized below,
    // and both pointers passed to sigaction remain valid for the call.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler;
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0
        || unsafe { libc::sigaction(signal, &action, ptr::null_mut()) } != 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn unblock_helper_signals() -> Result<()> {
    // exec preserves the signal mask, so do not rely on launchd's current mask.
    let mut signals: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: signals is live and initialized before it is passed to sigprocmask.
    if unsafe { libc::sigemptyset(&mut signals) } != 0
        || unsafe { libc::sigaddset(&mut signals, libc::SIGTERM) } != 0
        || unsafe { libc::sigaddset(&mut signals, libc::SIGALRM) } != 0
        || unsafe { libc::sigprocmask(libc::SIG_UNBLOCK, &signals, ptr::null_mut()) } != 0
    {
        return Err(io::Error::last_os_error()).context("unblock HID helper signals");
    }
    Ok(())
}

fn authenticate_client(stream: &mut UnixStream) -> Result<BufReader<UnixStream>> {
    stream.set_read_timeout(Some(HID_CONNECT_TIMEOUT))?;
    stream.set_write_timeout(Some(HID_CONNECT_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let hello = match read_message(&mut reader).context("read HID client handshake") {
        Ok(hello) => hello,
        Err(error) => {
            let _ = write_error(stream, HelperErrorCode::Helper, &error.to_string());
            return Err(error);
        }
    };
    if require_version(&hello).is_err()
        || hello.get("type").and_then(Value::as_str) != Some("hello")
        || hello.get("helperVersion").and_then(Value::as_str) != Some(HELPER_VERSION)
    {
        let _ = write_error(
            stream,
            HelperErrorCode::Helper,
            "incompatible HID client protocol",
        );
        bail!("incompatible HID client protocol")
    }
    Ok(reader)
}

fn serve_client(stream: &mut UnixStream, reader: BufReader<UnixStream>) -> Result<()> {
    let (event_tx, event_rx) = mpsc::channel();
    let device = match MicroDevice::open_exclusive(event_tx) {
        Ok(device) => device,
        Err(error) => {
            let _ = write_error(stream, HelperErrorCode::Unavailable, &error.to_string());
            return Err(error);
        }
    };
    write_message(
        stream,
        &json!({
            "v": HID_PROTOCOL_VERSION,
            "type": "hello",
            "helperVersion": HELPER_VERSION,
        }),
    )?;
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(DEFAULT_REQUEST_TIMEOUT))?;

    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let command_writer = Arc::clone(&writer);
    let close_in_progress = Arc::new(AtomicBool::new(false));
    let command_close_in_progress = Arc::clone(&close_in_progress);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let command_thread = thread::Builder::new()
        .name("herdr-micro-hid-commands".into())
        .spawn(move || {
            let result = serve_commands(reader, command_writer, device, command_close_in_progress);
            let _ = done_tx.send(result.map_err(|error| error.to_string()));
        })?;

    let mut shutting_down = false;
    let mut terminal_error = None;
    let result = loop {
        if HELPER_SHUTDOWN.load(Ordering::Acquire) {
            shutting_down = true;
            break Ok(());
        }
        match done_rx.try_recv() {
            Ok(result) => break result.map_err(anyhow::Error::msg),
            Err(mpsc::TryRecvError::Disconnected) => {
                break Err(anyhow!("HID command reader stopped"));
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match event_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(event) => {
                if let DeviceEvent::Disconnected { error } = &event {
                    terminal_error = Some(error.clone());
                }
                let message = json!({
                    "v": HID_PROTOCOL_VERSION,
                    "type": "event",
                    "event": event,
                });
                let sent = writer
                    .lock()
                    .map_err(|_| anyhow!("HID socket writer poisoned"))
                    .and_then(|mut writer| write_message(&mut *writer, &message));
                if let Err(error) = sent {
                    break Err(error);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break event_disconnect_result(
                    &done_rx,
                    &close_in_progress,
                    terminal_error.as_deref(),
                );
            }
        }
    };

    if let Err(error) = &result
        && let Ok(mut writer) = writer.lock()
    {
        let _ = write_error(
            &mut *writer,
            HelperErrorCode::Unavailable,
            &error.to_string(),
        );
    }
    // Wake the command reader without discarding a final device/error event.
    let _ = stream.shutdown(std::net::Shutdown::Read);
    let closed = if shutting_down {
        match done_rx.recv_timeout(HELPER_SHUTDOWN_TIMEOUT) {
            Ok(result) => result.map_err(anyhow::Error::msg),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow!("USB helper shutdown timed out")),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!("HID command reader stopped")),
        }
    } else {
        Ok(())
    };
    let joined = command_thread
        .join()
        .map_err(|_| anyhow!("HID command thread panicked"));
    drop(writer);
    combine_results([result, closed, joined])
}

fn event_disconnect_result(
    done_rx: &mpsc::Receiver<std::result::Result<(), String>>,
    close_in_progress: &AtomicBool,
    terminal_error: Option<&str>,
) -> Result<()> {
    if close_in_progress.load(Ordering::Acquire) {
        wait_for_command_completion(done_rx, HELPER_SHUTDOWN_TIMEOUT)
    } else {
        Err(anyhow!(
            terminal_error
                .unwrap_or("Codex Micro disconnected")
                .to_owned()
        ))
    }
}

fn wait_for_command_completion(
    done_rx: &mpsc::Receiver<std::result::Result<(), String>>,
    timeout: Duration,
) -> Result<()> {
    match done_rx.recv_timeout(timeout) {
        Ok(result) => result.map_err(anyhow::Error::msg),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow!("Codex Micro disconnected")),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!("HID command reader stopped")),
    }
}

fn serve_commands(
    mut reader: BufReader<UnixStream>,
    writer: Arc<Mutex<UnixStream>>,
    mut device: MicroDevice,
    close_in_progress: Arc<AtomicBool>,
) -> Result<()> {
    let result = (|| -> Result<()> {
        loop {
            let message = read_message(&mut reader)?;
            let command = parse_command(&message)?;
            match command {
                Incoming::Close { id } => {
                    close_in_progress.store(true, Ordering::Release);
                    match device.close() {
                        Ok(()) => {
                            write_reply(&writer, id, Ok(Value::Null))?;
                            return Ok(());
                        }
                        Err(error) => {
                            write_reply(&writer, id, Err(anyhow!(error.to_string())))?;
                            return Err(error);
                        }
                    }
                }
                Incoming::Send { id, method, params } => {
                    let result = if allowed_send(method) {
                        device.send(method, params).map(|()| Value::Null)
                    } else {
                        Err(anyhow!("HID send method is not allowed: {method}"))
                    };
                    write_reply(&writer, id, result)?;
                }
                Incoming::Request { id, method, params } => {
                    let result = if allowed_request(method, params.as_ref()) {
                        device.request(method, params)
                    } else {
                        Err(anyhow!("HID request is not allowed: {method}"))
                    };
                    write_reply(&writer, id, result)?;
                }
            }
        }
    })();
    let recovery = if result.is_err() {
        serde_json::to_value(layer_identity(1))
            .map_err(anyhow::Error::from)
            .and_then(|params| device.send("host.focused_app", Some(params)))
            .context("select native layer after HID client disconnect")
    } else {
        Ok(())
    };
    let close = device.close();
    combine_results([result, recovery, close])
}

fn combine_results<const N: usize>(results: [Result<()>; N]) -> Result<()> {
    let errors: Vec<_> = results
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        bail!(errors.join("; "))
    }
}

fn write_reply(writer: &Mutex<UnixStream>, id: u64, result: Result<Value>) -> Result<()> {
    let message = match result {
        Ok(result) => json!({
            "v": HID_PROTOCOL_VERSION,
            "type": "reply",
            "id": id,
            "result": result,
        }),
        Err(error) => json!({
            "v": HID_PROTOCOL_VERSION,
            "type": "reply",
            "id": id,
            "error": error.to_string(),
        }),
    };
    let mut writer = writer
        .lock()
        .map_err(|_| anyhow!("HID socket writer poisoned"))?;
    write_message(&mut *writer, &message)
}

fn write_error(stream: &mut impl Write, code: HelperErrorCode, error: &str) -> Result<()> {
    write_message(
        stream,
        &json!({
            "v": HID_PROTOCOL_VERSION,
            "type": "error",
            "code": code.as_str(),
            "error": error,
        }),
    )
}

enum Incoming<'a> {
    Send {
        id: u64,
        method: &'a str,
        params: Option<Value>,
    },
    Request {
        id: u64,
        method: &'a str,
        params: Option<Value>,
    },
    Close {
        id: u64,
    },
}

fn parse_command(message: &Value) -> Result<Incoming<'_>> {
    require_version(message)?;
    match message.get("type").and_then(Value::as_str) {
        Some("close") => Ok(Incoming::Close {
            id: message
                .get("id")
                .and_then(Value::as_u64)
                .filter(|id| *id != 0)
                .ok_or_else(|| anyhow!("HID close command is missing an id"))?,
        }),
        Some(kind @ ("send" | "request")) => {
            let id = message
                .get("id")
                .and_then(Value::as_u64)
                .filter(|id| *id != 0)
                .ok_or_else(|| anyhow!("HID command is missing an id"))?;
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("HID command is missing a method"))?;
            let params = message.get("params").filter(|v| !v.is_null()).cloned();
            if kind == "send" {
                Ok(Incoming::Send { id, method, params })
            } else {
                Ok(Incoming::Request { id, method, params })
            }
        }
        _ => bail!("unknown HID command"),
    }
}

fn allowed_request(method: &str, params: Option<&Value>) -> bool {
    match method {
        "device.status" => params.is_none(),
        "fs.readbin" | "fs.writebin" => {
            params
                .and_then(|value| value.get("file"))
                .and_then(Value::as_str)
                == Some("keymap.json")
        }
        _ => false,
    }
}

fn allowed_send(method: &str) -> bool {
    matches!(
        method,
        "v.oai.rgbcfg" | "v.oai.thstatus" | "host.focused_app"
    )
}

fn activated_listener() -> Result<UnixListener> {
    let name = CString::new(HID_LAUNCH_SOCKET_NAME).unwrap();
    let mut fds: *mut c_int = ptr::null_mut();
    let mut count = 0_usize;
    // SAFETY: launchd initializes the returned allocation and count on success.
    let status = unsafe { launch_activate_socket(name.as_ptr(), &mut fds, &mut count) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status)).context("activate launchd HID socket");
    }
    if count != 1 || fds.is_null() {
        if !fds.is_null() {
            for index in 0..count {
                // SAFETY: launchd returned count initialized descriptors.
                unsafe { libc::close(*fds.add(index)) };
            }
            // SAFETY: launchd allocated the descriptor array with malloc.
            unsafe { libc::free(fds.cast::<c_void>()) };
        }
        bail!("launchd Control socket supplied {count} descriptors")
    }
    // SAFETY: launchd returned one owned socket descriptor.
    let listener = unsafe { UnixListener::from_raw_fd(*fds) };
    // SAFETY: the descriptor value is copied into UnixListener; only the array is freed.
    unsafe { libc::free(fds.cast::<c_void>()) };
    Ok(listener)
}

unsafe extern "C" {
    fn launch_activate_socket(name: *const c_char, fds: *mut *mut c_int, cnt: *mut usize) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_socket(name: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "herdr-micro-hid-{name}-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn client_correlates_replies_and_dispatches_events() {
        let path = test_socket("client");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            assert_eq!(read_message(&mut reader).unwrap()["type"], "hello");
            write_message(
                &mut stream,
                &json!({
                    "v": HID_PROTOCOL_VERSION,
                    "type": "hello",
                    "helperVersion": HELPER_VERSION,
                }),
            )
            .unwrap();
            let request = read_message(&mut reader).unwrap();
            write_message(
                &mut stream,
                &json!({
                    "v": HID_PROTOCOL_VERSION,
                    "type": "event",
                    "event": {"type": "key", "key": "ACT06", "action": 1},
                }),
            )
            .unwrap();
            write_message(
                &mut stream,
                &json!({
                    "v": HID_PROTOCOL_VERSION,
                    "type": "reply",
                    "id": request["id"],
                    "result": {"device": "ok"},
                }),
            )
            .unwrap();
            let close = read_message(&mut reader).unwrap();
            assert_eq!(close["type"], "close");
            write_message(
                &mut stream,
                &json!({
                    "v": HID_PROTOCOL_VERSION,
                    "type": "reply",
                    "id": close["id"],
                    "result": null,
                }),
            )
            .unwrap();
        });
        let (events, event_rx) = mpsc::channel();
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        let mut client = HidClient::connect_at(&path, events, uid).unwrap();
        assert_eq!(
            client.request("device.status", None).unwrap(),
            json!({"device": "ok"})
        );
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            DeviceEvent::Key {
                key: "ACT06".into(),
                action: 1,
            }
        );
        client.close().unwrap();
        server.join().unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn client_rejects_the_wrong_server_uid() {
        let path = test_socket("peer");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || drop(listener.accept().unwrap()));
        let (events, _) = mpsc::channel();
        // SAFETY: getuid has no preconditions.
        let wrong_uid = unsafe { libc::getuid() }.wrapping_add(1);
        let error = HidClient::connect_at(&path, events, wrong_uid)
            .err()
            .unwrap();
        assert!(error.to_string().contains("unexpected uid"));
        server.join().unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn disconnect_notifies_once() {
        let path = test_socket("disconnect");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let _ = read_message(&mut reader).unwrap();
            write_message(
                &mut stream,
                &json!({
                    "v": HID_PROTOCOL_VERSION,
                    "type": "hello",
                    "helperVersion": HELPER_VERSION,
                }),
            )
            .unwrap();
            let _ = read_message(&mut reader);
        });
        let (events, event_rx) = mpsc::channel();
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        let mut client = HidClient::connect_at(&path, events, uid).unwrap();
        client.disconnect("write failed".into());
        client.disconnect("duplicate".into());
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            DeviceEvent::Disconnected {
                error: "write failed".into()
            }
        );
        assert!(event_rx.recv_timeout(Duration::from_millis(50)).is_err());
        client.close().unwrap();
        server.join().unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn client_rejects_a_stale_helper_version() {
        let path = test_socket("version");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let hello = read_message(&mut reader).unwrap();
            assert_eq!(hello["helperVersion"], HELPER_VERSION);
            write_message(
                &mut stream,
                &json!({
                    "v": HID_PROTOCOL_VERSION,
                    "type": "hello",
                    "helperVersion": "stale",
                }),
            )
            .unwrap();
        });
        let (events, _) = mpsc::channel();
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        let error = HidClient::connect_at(&path, events, uid).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("incompatible USB helper version")
        );
        server.join().unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn authentication_rejects_invalid_then_claims_one_valid_client() {
        let path = test_socket("one-shot");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        HELPER_SHUTDOWN.store(false, Ordering::Release);
        let mut invalid = UnixStream::connect(&path).unwrap();
        write_message(
            &mut invalid,
            &json!({"v": HID_PROTOCOL_VERSION, "type": "invalid"}),
        )
        .unwrap();
        let mut valid = UnixStream::connect(&path).unwrap();
        write_message(
            &mut valid,
            &json!({
                "v": HID_PROTOCOL_VERSION,
                "type": "hello",
                "helperVersion": HELPER_VERSION,
            }),
        )
        .unwrap();

        assert!(
            accept_authenticated_client(&listener, uid, Duration::from_secs(1))
                .unwrap()
                .is_some()
        );
        let error = read_message(&mut BufReader::new(invalid)).unwrap();
        assert_eq!(error["error"], "incompatible HID client protocol");
        HELPER_SHUTDOWN.store(false, Ordering::Release);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn one_shot_helper_stops_waiting_for_a_client() {
        let path = test_socket("idle");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        HELPER_SHUTDOWN.store(false, Ordering::Release);

        assert!(
            accept_authenticated_client(&listener, uid, Duration::from_millis(20))
                .unwrap()
                .is_none()
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn helper_error_codes_drive_device_state() {
        for (code, expected) in [
            (HelperErrorCode::Helper, "helper-error"),
            (HelperErrorCode::Unavailable, "unavailable"),
        ] {
            let error = validate_hello(&json!({
                "v": HID_PROTOCOL_VERSION,
                "type": "error",
                "code": code.as_str(),
                "error": "rejected",
            }))
            .unwrap_err();
            assert_eq!(connection_error_state(&error), expected);
        }
    }

    #[test]
    fn cleanup_errors_preserve_every_failure() {
        let error = combine_results([
            Err(anyhow!("primary")),
            Err(anyhow!("recovery")),
            Err(anyhow!("close")),
        ])
        .unwrap_err()
        .to_string();
        assert_eq!(error, "primary; recovery; close");
    }

    #[test]
    fn protocol_is_bounded_and_allowlists_are_exact() {
        assert!(read_line(&mut io::Cursor::new(vec![b'x'; HID_MAX_LINE_BYTES + 1])).is_err());
        assert!(allowed_request("device.status", None));
        assert!(allowed_request(
            "fs.readbin",
            Some(&json!({"file":"keymap.json"}))
        ));
        assert!(!allowed_request(
            "fs.writebin",
            Some(&json!({"file":"firmware.bin"}))
        ));
        assert!(!allowed_request("device.reset", None));
        assert!(allowed_send("host.focused_app"));
        assert!(!allowed_send("host.focused_app.extra"));
    }

    #[test]
    fn event_disconnect_waits_for_a_close_completion() {
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let completion = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            done_tx.send(Ok(())).unwrap();
        });

        assert!(event_disconnect_result(&done_rx, &AtomicBool::new(true), None).is_ok());
        completion.join().unwrap();
    }

    #[test]
    fn event_disconnect_without_a_close_is_reported_immediately() {
        let (_done_tx, done_rx) = mpsc::sync_channel(1);

        let error = event_disconnect_result(&done_rx, &AtomicBool::new(false), None).unwrap_err();
        assert_eq!(error.to_string(), "Codex Micro disconnected");
    }

    #[test]
    fn event_disconnect_preserves_the_device_error() {
        let (_done_tx, done_rx) = mpsc::sync_channel(1);

        let error = event_disconnect_result(
            &done_rx,
            &AtomicBool::new(false),
            Some("interrupt read failed: 0xe00002ed"),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "interrupt read failed: 0xe00002ed");
    }
}
