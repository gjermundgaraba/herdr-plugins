//! Privileged Codex Micro access over a local, versioned NDJSON socket.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    ffi::{CString, c_char, c_int, c_void},
    io::{self, Read, Write},
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

use crate::device::{DEFAULT_REQUEST_TIMEOUT, DEVICE_OPEN_TIMEOUT, DeviceEvent, MicroDevice};

pub const HID_PROTOCOL_VERSION: u32 = 1;
pub const HID_LAUNCH_SOCKET_NAME: &str = "Control";
pub const HID_MAX_LINE_BYTES: usize = 64 * 1024;
pub const HID_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
pub const HELPER_LABEL: &str = "dev.herdr.herdr-micro-hid";
// Increment whenever privileged helper code changes.
pub const HELPER_VERSION: &str = "2";
const HID_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const HELPER_SHUTDOWN_TIMEOUT: Duration = DEVICE_OPEN_TIMEOUT;
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
        let hello = read_message(&mut stream).context("read USB helper handshake")?;
        validate_hello(&hello)?;
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(Some(DEFAULT_REQUEST_TIMEOUT))?;

        let read_stream = stream.try_clone()?;
        let writer = Arc::new(Mutex::new(stream));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let reader_pending = Arc::clone(&pending);
        let reader_closed = Arc::clone(&closed);
        let reader = thread::Builder::new()
            .name("herdr-micro-hid-ipc".into())
            .spawn({
                let event_tx = event_tx.clone();
                move || client_reader(read_stream, event_tx, reader_pending, reader_closed)
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
        self.call("send", method.into(), params, DEFAULT_REQUEST_TIMEOUT)
            .map(|_| ())
    }

    pub fn request(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value> {
        self.call("request", method.into(), params, timeout)
    }

    fn call(
        &self,
        kind: &'static str,
        method: String,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value> {
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
        let wait = timeout
            .checked_add(Duration::from_millis(200))
            .unwrap_or(timeout);
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
        if let Ok(writer) = self.writer.lock() {
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
        if notify {
            let _ = self.event_tx.send(DeviceEvent::Disconnected { error });
        }
    }

    pub fn close(&mut self) -> Result<()> {
        let result = if !self.closed.swap(true, Ordering::AcqRel) {
            self.close_remote()
        } else {
            Ok(())
        };
        if let Ok(writer) = self.writer.lock() {
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
        if self
            .reader
            .take()
            .map(JoinHandle::join)
            .transpose()
            .is_err()
        {
            return Err(anyhow!("HID IPC reader thread panicked"));
        }
        result
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

impl crate::device::keymap::Requester for HidClient {
    fn request(&self, method: &str, params: Option<Value>, timeout: Duration) -> Result<Value> {
        HidClient::request(self, method, params, timeout)
    }
}

fn client_reader(
    mut stream: UnixStream,
    event_tx: Sender<DeviceEvent>,
    pending: Arc<Mutex<HashMap<u64, SyncSender<Reply>>>>,
    closed: Arc<AtomicBool>,
) {
    let result = (|| -> Result<()> {
        loop {
            let message = read_message(&mut stream)?;
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
        Some("error") => bail!(
            "{}",
            message
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("USB helper rejected the connection")
        ),
        _ => bail!("invalid USB helper handshake"),
    }
}

fn require_version(message: &Value) -> Result<()> {
    if message.get("v").and_then(Value::as_u64) != Some(u64::from(HID_PROTOCOL_VERSION)) {
        bail!("incompatible USB helper protocol")
    }
    Ok(())
}

fn read_message(stream: &mut impl Read) -> Result<Value> {
    let line = read_line(stream)?;
    serde_json::from_slice(&line).context("parse HID IPC message")
}

fn read_line(stream: &mut impl Read) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => bail!("HID IPC connection closed"),
            Ok(_) if byte[0] == b'\n' => return Ok(line),
            Ok(_) if line.len() < HID_MAX_LINE_BYTES => line.push(byte[0]),
            Ok(_) => bail!("HID IPC message exceeds {HID_MAX_LINE_BYTES} bytes"),
            Err(error) => return Err(error.into()),
        }
    }
}

fn write_message(stream: &mut impl Write, message: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(message)?;
    if bytes.len() > HID_MAX_LINE_BYTES {
        bail!("HID IPC message exceeds {HID_MAX_LINE_BYTES} bytes")
    }
    stream.write_all(&bytes)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
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
    // SAFETY: the handler only stores to a lock-free atomic flag.
    if unsafe {
        libc::signal(
            libc::SIGTERM,
            request_helper_shutdown as *const () as libc::sighandler_t,
        )
    } == libc::SIG_ERR
    {
        return Err(io::Error::last_os_error()).context("install SIGTERM handler");
    }
    listener.set_nonblocking(true)?;
    let active = Arc::new(AtomicBool::new(false));
    let mut worker: Option<JoinHandle<()>> = None;
    let mut idle_since = Instant::now();
    loop {
        if HELPER_SHUTDOWN.load(Ordering::Acquire) {
            if active.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            break;
        }
        if worker.as_ref().is_some_and(JoinHandle::is_finished) {
            if worker.take().unwrap().join().is_err() {
                return Err(anyhow!("HID client worker panicked"));
            }
            idle_since = Instant::now();
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .context("make accepted HID client blocking")?;
                let peer = match peer_euid(&stream) {
                    Ok(peer) => peer,
                    Err(error) => {
                        let _ = write_error(&mut stream, &error.to_string());
                        continue;
                    }
                };
                if peer != allowed_uid {
                    let _ = write_error(&mut stream, "unauthorized HID client");
                    continue;
                }
                if active
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    let _ = write_error(&mut stream, "HID device is busy");
                    continue;
                }
                if let Some(previous) = worker.take() {
                    let _ = previous.join();
                }
                let lease = Arc::clone(&active);
                worker = Some(
                    thread::Builder::new()
                        .name("herdr-micro-hid-client".into())
                        .spawn(move || {
                            let _guard = LeaseGuard(lease);
                            let _ = serve_client(&mut stream);
                        })?,
                );
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if !active.load(Ordering::Acquire) && idle_since.elapsed() >= HID_IDLE_TIMEOUT {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error).context("accept HID client"),
        }
    }
    if let Some(worker) = worker {
        let _ = worker.join();
    }
    Ok(())
}

struct LeaseGuard(Arc<AtomicBool>);

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn serve_client(stream: &mut UnixStream) -> Result<()> {
    stream.set_read_timeout(Some(HID_CONNECT_TIMEOUT))?;
    stream.set_write_timeout(Some(HID_CONNECT_TIMEOUT))?;
    let hello = match read_message(stream).context("read HID client handshake") {
        Ok(hello) => hello,
        Err(error) => {
            let _ = write_error(stream, &error.to_string());
            return Err(error);
        }
    };
    if require_version(&hello).is_err()
        || hello.get("type").and_then(Value::as_str) != Some("hello")
        || hello.get("helperVersion").and_then(Value::as_str) != Some(HELPER_VERSION)
    {
        let _ = write_error(stream, "incompatible HID client protocol");
        bail!("incompatible HID client protocol")
    }

    let (event_tx, event_rx) = mpsc::channel();
    let device = match MicroDevice::open_exclusive(event_tx) {
        Ok(device) => device,
        Err(error) => {
            let _ = write_error(stream, &error.to_string());
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

    let reader = stream.try_clone()?;
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
                break event_disconnect_result(&done_rx, &close_in_progress);
            }
        }
    };

    if let Err(error) = &result {
        if let Ok(mut writer) = writer.lock() {
            let _ = write_error(&mut *writer, &error.to_string());
        }
    }
    // Wake the command reader without discarding a final device/error event.
    let _ = stream.shutdown(std::net::Shutdown::Read);
    if shutting_down {
        let closed = done_rx
            .recv_timeout(HELPER_SHUTDOWN_TIMEOUT)
            .map_err(|_| anyhow!("USB helper shutdown timed out"))?
            .map_err(anyhow::Error::msg);
        let joined = command_thread
            .join()
            .map_err(|_| anyhow!("HID command thread panicked"));
        drop(writer);
        return result.and(closed).and(joined);
    }
    let joined = command_thread
        .join()
        .map_err(|_| anyhow!("HID command thread panicked"));
    drop(writer);
    result.and(joined)
}

fn event_disconnect_result(
    done_rx: &mpsc::Receiver<std::result::Result<(), String>>,
    close_in_progress: &AtomicBool,
) -> Result<()> {
    if close_in_progress.load(Ordering::Acquire) {
        wait_for_command_completion(done_rx, HELPER_SHUTDOWN_TIMEOUT)
    } else {
        Err(anyhow!("Codex Micro disconnected"))
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
    mut reader: UnixStream,
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
                        device.request(method, params, DEFAULT_REQUEST_TIMEOUT)
                    } else {
                        Err(anyhow!("HID request is not allowed: {method}"))
                    };
                    write_reply(&writer, id, result)?;
                }
            }
        }
    })();
    result.and(device.close())
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

fn write_error(stream: &mut impl Write, error: &str) -> Result<()> {
    write_message(
        stream,
        &json!({
            "v": HID_PROTOCOL_VERSION,
            "type": "error",
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
            assert_eq!(read_message(&mut stream).unwrap()["type"], "hello");
            write_message(
                &mut stream,
                &json!({
                    "v": HID_PROTOCOL_VERSION,
                    "type": "hello",
                    "helperVersion": HELPER_VERSION,
                }),
            )
            .unwrap();
            let request = read_message(&mut stream).unwrap();
            write_message(
                &mut stream,
                &json!({
                    "v": HID_PROTOCOL_VERSION,
                    "type": "event",
                    "event": {"type": "key", "key": "F13", "action": 1},
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
            let close = read_message(&mut stream).unwrap();
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
            client
                .request("device.status", None, Duration::from_secs(1))
                .unwrap(),
            json!({"device": "ok"})
        );
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            DeviceEvent::Key {
                key: "F13".into(),
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
            let _ = read_message(&mut stream).unwrap();
            write_message(
                &mut stream,
                &json!({
                    "v": HID_PROTOCOL_VERSION,
                    "type": "hello",
                    "helperVersion": HELPER_VERSION,
                }),
            )
            .unwrap();
            let _ = read_message(&mut stream);
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
            let hello = read_message(&mut stream).unwrap();
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
        let (event_tx, event_rx) = mpsc::channel::<DeviceEvent>();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        drop(event_tx);
        assert_eq!(
            event_rx.recv_timeout(Duration::from_millis(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
        let completion = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            done_tx.send(Ok(())).unwrap();
        });

        assert!(event_disconnect_result(&done_rx, &AtomicBool::new(true)).is_ok());
        completion.join().unwrap();
    }

    #[test]
    fn event_disconnect_without_a_close_is_reported_immediately() {
        let (event_tx, event_rx) = mpsc::channel::<DeviceEvent>();
        let (_done_tx, done_rx) = mpsc::sync_channel(1);
        drop(event_tx);
        assert_eq!(
            event_rx.recv_timeout(Duration::from_millis(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );

        let error = event_disconnect_result(&done_rx, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(error.to_string(), "Codex Micro disconnected");
    }
}
