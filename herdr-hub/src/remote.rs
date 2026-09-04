use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, BufReader, Read, Write},
    os::unix::process::CommandExt,
    os::unix::{io::AsRawFd, net::UnixStream},
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use herdr_client::ndjson;
use herdr_hub_client::{ClientMessage, Model, PROTOCOL, ServerMessage, SessionState};
use serde::Serialize;
use serde_json::Value;

use crate::config::HostConfig;

const COMMAND_QUEUE: usize = 64;
const RELAY_QUEUE: usize = 64;
const MAX_IN_FLIGHT: usize = 64;
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const EVENT_RETRY: Duration = Duration::from_millis(10);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(crate) enum Event {
    Connected {
        host: String,
        epoch: u64,
        sessions: Vec<SessionState>,
    },
    Session {
        host: String,
        epoch: u64,
        session: SessionState,
    },
    SessionRemoved {
        host: String,
        epoch: u64,
        key: String,
    },
    Disconnected {
        host: String,
        epoch: u64,
        error: String,
    },
    Reply {
        host: String,
        epoch: u64,
        token: u64,
        result: std::result::Result<Value, String>,
    },
}

pub(crate) struct Call {
    pub epoch: u64,
    pub token: u64,
    pub session: String,
    pub method: String,
    pub params: Value,
    pub caller: UnixStream,
}

struct HostHandle {
    commands: SyncSender<Call>,
    wake: UnixStream,
    epoch: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl HostHandle {
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        wake(&self.wake);
    }

    fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HostHandle {
    fn drop(&mut self) {
        self.stop();
        self.join();
    }
}

pub(crate) struct Supervisor {
    hosts: HashMap<String, HostHandle>,
}

impl Supervisor {
    pub(crate) fn spawn(hosts: Vec<HostConfig>, events: SyncSender<Event>) -> Result<Self> {
        Self::spawn_with_program(hosts, events, "ssh".into())
    }

    fn spawn_with_program(
        hosts: Vec<HostConfig>,
        events: SyncSender<Event>,
        ssh_program: OsString,
    ) -> Result<Self> {
        let mut handles = HashMap::new();
        for host in hosts {
            let key = host.key.clone();
            if handles.contains_key(&key) {
                bail!("duplicate remote host key {key:?}")
            }
            let (commands, command_rx) = mpsc::sync_channel(COMMAND_QUEUE);
            let (wake_rx, wake) = UnixStream::pair().context("create remote-host wake socket")?;
            wake_rx.set_nonblocking(true)?;
            wake.set_nonblocking(true)?;
            let worker_wake = wake.try_clone()?;
            let epoch = Arc::new(AtomicU64::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let worker_epoch = Arc::clone(&epoch);
            let worker_stop = Arc::clone(&stop);
            let worker_events = events.clone();
            let worker_program = ssh_program.clone();
            let thread = thread::Builder::new()
                .name("herdr-hub-remote".into())
                .spawn(move || {
                    HostWorker {
                        host,
                        ssh_program: worker_program,
                        commands: command_rx,
                        wake_rx,
                        wake_tx: worker_wake,
                        events: worker_events,
                        current_epoch: worker_epoch,
                        stop: worker_stop,
                    }
                    .run();
                })
                .context("start remote-host supervisor")?;
            handles.insert(
                key,
                HostHandle {
                    commands,
                    wake,
                    epoch,
                    stop,
                    thread: Some(thread),
                },
            );
        }
        Ok(Self { hosts: handles })
    }

    pub(crate) fn call(&self, host: &str, mut call: Call) -> Result<()> {
        let handle = self
            .hosts
            .get(host)
            .ok_or_else(|| anyhow!("unknown remote host {host:?}"))?;
        call.session = relay_session_key(host, &call.session)?;
        let current_epoch = handle.epoch.load(Ordering::Acquire);
        if current_epoch == 0 {
            bail!("remote host {host:?} is disconnected")
        }
        if current_epoch != call.epoch {
            bail!("remote host {host:?} connection changed")
        }
        match handle.commands.try_send(call) {
            Ok(()) => {
                wake(&handle.wake);
                Ok(())
            }
            Err(TrySendError::Full(_)) => bail!("remote host {host:?} call queue is full"),
            Err(TrySendError::Disconnected(_)) => {
                bail!("remote host {host:?} supervisor stopped")
            }
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        for handle in self.hosts.values() {
            handle.stop();
        }
        for handle in self.hosts.values_mut() {
            handle.join();
        }
    }
}

pub(crate) fn check_version(host: &HostConfig) -> Result<String> {
    check_version_with_program(host, std::ffi::OsStr::new("ssh"))
}

fn check_version_with_program(host: &HostConfig, program: &std::ffi::OsStr) -> Result<String> {
    check_version_with_program_and_timeout(host, program, VERSION_TIMEOUT)
}

fn check_version_with_program_and_timeout(
    host: &HostConfig,
    program: &std::ffi::OsStr,
    timeout: Duration,
) -> Result<String> {
    let output = run_version_command(version_command(program, host), timeout)
        .with_context(|| format!("check herdr-hub on remote host {:?}", host.key))?;
    if !output.status.success() {
        bail!(
            "remote host {:?} version check failed ({}): {}",
            host.key,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    let expected = format!("herdr-hub {}\n", env!("CARGO_PKG_VERSION"));
    if output.stdout != expected.as_bytes() {
        bail!(
            "remote host {:?} returned unexpected version {:?}; expected {:?}",
            host.key,
            String::from_utf8_lossy(&output.stdout),
            expected.trim_end()
        )
    }
    Ok(expected.trim_end().into())
}

fn version_command(program: &std::ffi::OsStr, host: &HostConfig) -> Command {
    let mut command = Command::new(program);
    add_ssh_connect_options(&mut command);
    command.args([host.ssh.as_str(), "herdr-hub", "--version"]);
    command
}

fn run_version_command(mut command: Command, timeout: Duration) -> Result<Output> {
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run {program}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        BufReader::new(stdout)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        BufReader::new(stderr)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child
            .try_wait()
            .with_context(|| format!("wait for {program}"))
        {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                stop_child(&mut child);
                break Err(anyhow!(
                    "{program} timed out after {}s",
                    timeout.as_secs_f64()
                ));
            }
            Err(error) => {
                stop_child(&mut child);
                break Err(error);
            }
        }
    };
    if status.is_ok() {
        terminate_process_group(&child);
    }
    let stdout = join_reader(stdout_reader, "stdout", &program)?;
    let stderr = join_reader(stderr_reader, "stderr", &program)?;
    Ok(Output {
        status: status?,
        stdout,
        stderr,
    })
}

fn join_reader(
    reader: JoinHandle<io::Result<Vec<u8>>>,
    stream: &str,
    program: &str,
) -> Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| anyhow!("{stream} reader for {program} panicked"))?
        .with_context(|| format!("read {stream} from {program}"))
}

struct HostWorker {
    host: HostConfig,
    ssh_program: OsString,
    commands: Receiver<Call>,
    wake_rx: UnixStream,
    wake_tx: UnixStream,
    events: SyncSender<Event>,
    current_epoch: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

struct ConnectionFailure {
    connected: bool,
    error: anyhow::Error,
}

impl HostWorker {
    fn run(&self) {
        let mut epoch = 0_u64;
        let mut backoff = MIN_BACKOFF;
        while !self.stop.load(Ordering::Acquire) {
            epoch = epoch
                .checked_add(1)
                .expect("remote connection epoch overflowed");
            let outcome = self.run_connection(epoch);
            self.current_epoch.store(0, Ordering::Release);
            if self.stop.load(Ordering::Acquire) {
                break;
            }
            let (connected, error) = match outcome {
                Ok(connected) => (connected, "relay disconnected".to_owned()),
                Err(failure) => (failure.connected, format!("{:#}", failure.error)),
            };
            if !send_event(
                &self.events,
                &self.stop,
                Event::Disconnected {
                    host: self.host.key.clone(),
                    epoch,
                    error,
                },
            ) {
                break;
            }
            if connected {
                backoff = MIN_BACKOFF;
            }
            if !wait_to_reconnect(
                &self.commands,
                &self.wake_rx,
                &self.events,
                &self.host.key,
                &self.stop,
                backoff,
            ) {
                break;
            }
            backoff = next_backoff(backoff);
        }
        self.current_epoch.store(0, Ordering::Release);
    }

    fn run_connection(&self, epoch: u64) -> std::result::Result<bool, ConnectionFailure> {
        let mut child = match spawn_relay(&self.ssh_program, &self.host) {
            Ok(child) => child,
            Err(error) => {
                return Err(ConnectionFailure {
                    connected: false,
                    error,
                });
            }
        };
        let stdin = child.stdin.take().expect("piped stdin");
        if let Err(error) = set_nonblocking(stdin.as_raw_fd()) {
            stop_child(&mut child);
            return Err(ConnectionFailure {
                connected: false,
                error: anyhow!("make relay stdin nonblocking: {error}"),
            });
        }
        let stdout = child.stdout.take().expect("piped stdout");
        let (incoming_tx, incoming) = mpsc::sync_channel(RELAY_QUEUE);
        let reader_wake = match self.wake_tx.try_clone() {
            Ok(wake) => wake,
            Err(error) => {
                stop_child(&mut child);
                return Err(ConnectionFailure {
                    connected: false,
                    error: error.into(),
                });
            }
        };
        let reader = match thread::Builder::new()
            .name("herdr-hub-remote-reader".into())
            .spawn(move || read_relay(stdout, incoming_tx, reader_wake))
        {
            Ok(reader) => reader,
            Err(error) => {
                stop_child(&mut child);
                return Err(ConnectionFailure {
                    connected: false,
                    error: error.into(),
                });
            }
        };

        let mut connection = Connection {
            host: &self.host,
            epoch,
            commands: &self.commands,
            wake: &self.wake_rx,
            events: &self.events,
            current_epoch: &self.current_epoch,
            stop: &self.stop,
            child,
            stdin,
            incoming,
            imported: HashMap::new(),
            in_flight: HashMap::new(),
            next_counter: 1,
            connected: false,
        };
        let result = connection.run();
        self.current_epoch
            .compare_exchange(epoch, 0, Ordering::AcqRel, Ordering::Acquire)
            .ok();
        if !self.stop.load(Ordering::Acquire) {
            let error = result.as_ref().err().map_or_else(
                || "relay disconnected".to_owned(),
                |error| format!("{error:#}"),
            );
            connection.fail_in_flight(&error);
        }
        stop_child(&mut connection.child);
        drop(connection.incoming);
        let _ = reader.join();
        match result {
            Ok(()) => Ok(connection.connected),
            Err(error) => Err(ConnectionFailure {
                connected: connection.connected,
                error,
            }),
        }
    }
}

struct Connection<'a> {
    host: &'a HostConfig,
    epoch: u64,
    commands: &'a Receiver<Call>,
    wake: &'a UnixStream,
    events: &'a SyncSender<Event>,
    current_epoch: &'a AtomicU64,
    stop: &'a AtomicBool,
    child: Child,
    stdin: ChildStdin,
    incoming: Receiver<Incoming>,
    imported: HashMap<String, String>,
    in_flight: HashMap<u64, u64>,
    next_counter: u64,
    connected: bool,
}

impl Connection<'_> {
    fn run(&mut self) -> Result<()> {
        let hello = self.wait_for_hello()?;
        let sessions = self.import_initial(hello)?;
        self.current_epoch.store(self.epoch, Ordering::Release);
        if !send_event(
            self.events,
            self.stop,
            Event::Connected {
                host: self.host.key.clone(),
                epoch: self.epoch,
                sessions,
            },
        ) {
            return Ok(());
        }
        self.connected = true;

        loop {
            if self.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            self.apply_incoming()?;
            self.forward_calls()?;
            wait_for_wake(self.wake, None)?;
        }
    }

    fn wait_for_hello(&mut self) -> Result<Model> {
        loop {
            if self.stop.load(Ordering::Acquire) {
                bail!("remote supervisor stopped")
            }
            self.reject_queued_calls("remote connection changed")?;
            match self.incoming.try_recv() {
                Ok(Incoming::Message(message)) => match *message {
                    ServerMessage::Hello { protocol, model } if protocol == PROTOCOL => {
                        return Ok(model);
                    }
                    ServerMessage::Hello { protocol, .. } => {
                        bail!("remote hub protocol {protocol} does not match {PROTOCOL}")
                    }
                    ServerMessage::Error { error } => bail!(error),
                    _ => bail!("relay did not begin with Hello"),
                },
                Ok(Incoming::End(reason)) => bail!(reason),
                Err(TryRecvError::Empty) => wait_for_wake(self.wake, None)?,
                Err(TryRecvError::Disconnected) => {
                    bail!("relay stdout reader stopped before Hello")
                }
            }
        }
    }

    fn apply_incoming(&mut self) -> Result<()> {
        for _ in 0..RELAY_QUEUE {
            match self.incoming.try_recv() {
                Ok(Incoming::Message(message)) => self.apply_message(*message)?,
                Ok(Incoming::End(reason)) => bail!(reason),
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => bail!("relay stdout reader stopped"),
            }
        }
        Ok(())
    }

    fn import_initial(&mut self, model: Model) -> Result<Vec<SessionState>> {
        let mut sessions = Vec::new();
        for session in model.sessions {
            let Some((remote_key, session)) = rewrite_session(&self.host.key, session) else {
                continue;
            };
            if self
                .imported
                .insert(remote_key, session.key.clone())
                .is_some()
            {
                bail!("relay Hello contains a duplicate local session")
            }
            sessions.push(session);
        }
        sessions.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(sessions)
    }

    fn forward_calls(&mut self) -> Result<()> {
        while self.in_flight.len() < MAX_IN_FLIGHT {
            match self.commands.try_recv() {
                Ok(call) if call.epoch == self.epoch => {
                    let id = self.next_counter;
                    self.next_counter = self
                        .next_counter
                        .checked_add(1)
                        .ok_or_else(|| anyhow!("relay call counter overflowed"))?;
                    self.in_flight.insert(id, call.token);
                    if !self.write_message(
                        &ClientMessage::Call {
                            protocol: PROTOCOL,
                            id,
                            session: call.session,
                            method: call.method,
                            params: call.params,
                        },
                        &call.caller,
                    )? {
                        self.finish_call(
                            id,
                            Err("caller disconnected before call was sent".into()),
                        )?;
                    }
                }
                Ok(call) => self.reply(
                    call.epoch,
                    call.token,
                    Err("remote connection changed before call was sent".into()),
                )?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => bail!("remote call channel closed"),
            }
        }
        Ok(())
    }

    fn reject_queued_calls(&self, error: &str) -> Result<()> {
        loop {
            match self.commands.try_recv() {
                Ok(call) => self.reply(call.epoch, call.token, Err(error.into()))?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => bail!("remote call channel closed"),
            }
        }
    }

    fn write_message(&mut self, message: &impl Serialize, caller: &UnixStream) -> Result<bool> {
        let bytes = encode_message(message)?;
        let mut remaining = bytes.as_slice();
        while !remaining.is_empty() {
            if self.stop.load(Ordering::Acquire) {
                bail!("remote supervisor stopped while writing relay message")
            }
            if !caller_connected(caller) {
                if remaining.len() == bytes.len() {
                    return Ok(false);
                }
                // A partial frame cannot be retracted or followed by another call.
                bail!("caller disconnected while writing relay message")
            }
            match self.stdin.write(remaining) {
                Ok(0) => bail!("relay stdin closed while writing message"),
                Ok(written) => remaining = &remaining[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if !wait_for_writable(&self.stdin, self.wake, self.stop)? {
                        self.apply_incoming()?;
                    }
                }
                Err(error) => return Err(error).context("write relay message"),
            }
        }
        Ok(true)
    }

    fn apply_message(&mut self, message: ServerMessage) -> Result<()> {
        match message {
            ServerMessage::Session { session, .. } => {
                let Some((remote_key, session)) = rewrite_session(&self.host.key, session) else {
                    return Ok(());
                };
                self.imported.insert(remote_key, session.key.clone());
                self.emit_required(Event::Session {
                    host: self.host.key.clone(),
                    epoch: self.epoch,
                    session,
                })
            }
            ServerMessage::SessionRemoved { key, .. } => {
                let Some(key) = self.imported.remove(&key) else {
                    return Ok(());
                };
                self.emit_required(Event::SessionRemoved {
                    host: self.host.key.clone(),
                    epoch: self.epoch,
                    key,
                })
            }
            ServerMessage::Reply { id, result } => self.finish_call(id, Ok(result)),
            ServerMessage::ReplyError { id, error } => self.finish_call(id, Err(error)),
            ServerMessage::Error { error } => bail!(error),
            ServerMessage::Hello { .. } => bail!("relay sent a second Hello"),
            ServerMessage::Host { .. } | ServerMessage::Active { .. } => Ok(()),
        }
    }

    fn finish_call(&mut self, id: u64, result: std::result::Result<Value, String>) -> Result<()> {
        let token = self
            .in_flight
            .remove(&id)
            .ok_or_else(|| anyhow!("relay replied with unknown call id {id}"))?;
        self.reply(self.epoch, token, result)
    }

    fn fail_in_flight(&mut self, error: &str) {
        let calls = std::mem::take(&mut self.in_flight);
        for (_, token) in calls {
            let _ = self.reply(self.epoch, token, Err(error.into()));
        }
    }

    fn reply(
        &self,
        epoch: u64,
        token: u64,
        result: std::result::Result<Value, String>,
    ) -> Result<()> {
        self.emit_required(Event::Reply {
            host: self.host.key.clone(),
            epoch,
            token,
            result,
        })
    }

    fn emit_required(&self, event: Event) -> Result<()> {
        send_event(self.events, self.stop, event)
            .then_some(())
            .ok_or_else(|| anyhow!("hub stopped"))
    }
}

fn caller_connected(caller: &UnixStream) -> bool {
    loop {
        // A zero-byte send checks the reply side without changing the wire stream.
        // Read EOF alone would also reject callers that only half-close their input.
        // SAFETY: the socket is valid; the null buffer has zero length. Suppress SIGPIPE.
        let sent = unsafe {
            libc::send(
                caller.as_raw_fd(),
                std::ptr::null(),
                0,
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            )
        };
        if sent >= 0 {
            return true;
        }
        match io::Error::last_os_error().kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return true,
            _ => return false,
        }
    }
}

enum Incoming {
    Message(Box<ServerMessage>),
    End(String),
}

fn read_relay(stdout: impl Read, incoming: SyncSender<Incoming>, wake_tx: UnixStream) {
    let mut reader = BufReader::new(stdout);
    let mut pending = Vec::new();
    loop {
        let next = match ndjson::read_frame(&mut reader, &mut pending) {
            Ok(Some(frame)) => match serde_json::from_slice(&frame) {
                Ok(message) => Incoming::Message(Box::new(message)),
                Err(error) => Incoming::End(format!("decode relay message: {error}")),
            },
            Ok(None) => Incoming::End("relay closed stdout".into()),
            Err(error) => Incoming::End(format!("read relay message: {error}")),
        };
        let ended = matches!(next, Incoming::End(_));
        if incoming.send(next).is_err() {
            return;
        }
        wake(&wake_tx);
        if ended {
            return;
        }
    }
}

fn rewrite_session(host: &str, mut session: SessionState) -> Option<(String, SessionState)> {
    if session.host != "local" {
        return None;
    }
    let name = local_session_name(&session.key)?.to_owned();
    if name != session.name {
        return None;
    }
    let remote_key = session.key;
    session.host = host.into();
    session.key = format!("{host}/{name}");
    session.socket_path = None;
    session.client_focused = None;
    Some((remote_key, session))
}

fn local_session_name(key: &str) -> Option<&str> {
    let name = key.strip_prefix("local/")?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

fn relay_session_key(host: &str, key: &str) -> Result<String> {
    let prefix = format!("{host}/");
    let name = key
        .strip_prefix(&prefix)
        .filter(|name| !name.is_empty() && !name.contains('/'))
        .ok_or_else(|| anyhow!("session {key:?} does not belong to remote host {host:?}"))?;
    Ok(format!("local/{name}"))
}

fn next_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_BACKOFF)
}

fn wait_to_reconnect(
    commands: &Receiver<Call>,
    wake_rx: &UnixStream,
    events: &SyncSender<Event>,
    host: &str,
    stop: &AtomicBool,
    duration: Duration,
) -> bool {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Acquire) {
        loop {
            match commands.try_recv() {
                Ok(call) => {
                    if !send_event(
                        events,
                        stop,
                        Event::Reply {
                            host: host.into(),
                            epoch: call.epoch,
                            token: call.token,
                            result: Err("remote host disconnected before call was sent".into()),
                        },
                    ) {
                        return false;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return true;
        };
        if wait_for_wake(wake_rx, Some(remaining)).is_err() {
            return false;
        }
    }
    false
}

fn wait_for_wake(wake_rx: &UnixStream, timeout: Option<Duration>) -> io::Result<()> {
    let timeout = timeout.map_or(-1, |duration| {
        duration.as_millis().saturating_add(1).min(i32::MAX as u128) as i32
    });
    let mut descriptor = libc::pollfd {
        fd: wake_rx.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: `descriptor` points to one initialized pollfd for the call.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if result >= 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    drain_wake(wake_rx)
}

fn drain_wake(wake_rx: &UnixStream) -> io::Result<()> {
    let mut bytes = [0_u8; 256];
    loop {
        match (&*wake_rx).read(&mut bytes) {
            Ok(0) => return Err(io::ErrorKind::BrokenPipe.into()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn wait_for_writable(writer: &ChildStdin, wake_rx: &UnixStream, stop: &AtomicBool) -> Result<bool> {
    let mut descriptors = [
        libc::pollfd {
            fd: writer.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        },
        libc::pollfd {
            fd: wake_rx.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: `descriptors` is an initialized two-element pollfd array.
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("wait for relay stdin");
        }
        if descriptors[1].revents != 0 {
            drain_wake(wake_rx).context("drain remote-host wake socket")?;
            if stop.load(Ordering::Acquire) {
                bail!("remote supervisor stopped while writing relay message")
            }
            return Ok(false);
        }
        let writer_events = descriptors[0].revents;
        if writer_events & libc::POLLOUT != 0 {
            return Ok(true);
        }
        if writer_events & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            bail!("relay stdin closed while waiting to write")
        }
    }
}

fn set_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: `fd` is an open child-pipe descriptor and both fcntl operations
    // preserve all existing status flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Same valid descriptor; F_SETFL consumes the integer flag mask.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn wake(wake_tx: &UnixStream) {
    match (&*wake_tx).write(&[1]) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        Err(_) => {}
    }
}

fn send_event(events: &SyncSender<Event>, stop: &AtomicBool, mut event: Event) -> bool {
    loop {
        match events.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                if stop.load(Ordering::Acquire) {
                    return false;
                }
                event = returned;
                thread::sleep(EVENT_RETRY);
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn spawn_relay(ssh_program: &std::ffi::OsStr, host: &HostConfig) -> Result<Child> {
    let mut command = relay_command(ssh_program, host);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .with_context(|| format!("spawn relay for remote host {:?}", host.key))
}

fn relay_command(ssh_program: &std::ffi::OsStr, host: &HostConfig) -> Command {
    let mut command = Command::new(ssh_program);
    add_ssh_connect_options(&mut command);
    command.args([
        "-o",
        "ServerAliveInterval=15",
        host.ssh.as_str(),
        "herdr-hub",
        "relay",
    ]);
    command
}

fn add_ssh_connect_options(command: &mut Command) {
    command.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "ConnectionAttempts=1",
    ]);
}

fn encode_message(message: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(message)?;
    if bytes.len() + 1 > ndjson::MAX_FRAME_BYTES {
        bail!("relay message exceeds 1 MiB")
    }
    bytes.push(b'\n');
    Ok(bytes)
}

fn stop_child(child: &mut Child) {
    terminate_process_group(child);
    let _ = child.kill();
    let _ = child.wait();
}

fn terminate_process_group(child: &Child) {
    if let Ok(process_group) = i32::try_from(child.id()) {
        // SAFETY: relay and version-check children start their own process groups.
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use herdr_hub_client::{HostState, Model};
    use serde_json::json;

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    fn host() -> HostConfig {
        HostConfig {
            key: "workbox".into(),
            ssh: "ssh-target".into(),
        }
    }

    fn call(token: u64, method: &str, params: Value, caller: &UnixStream) -> Call {
        Call {
            epoch: 1,
            token,
            session: "workbox/default".into(),
            method: method.into(),
            params,
            caller: caller.try_clone().unwrap(),
        }
    }

    #[test]
    fn caller_connection_accepts_write_half_close_but_rejects_closed_peer() {
        let (caller, peer) = UnixStream::pair().unwrap();
        assert!(caller_connected(&caller));
        peer.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(caller_connected(&caller));
        drop(peer);
        assert!(!caller_connected(&caller));
    }

    fn session(key: &str, host: &str, name: &str) -> SessionState {
        SessionState {
            key: key.into(),
            host: host.into(),
            name: name.into(),
            connected: true,
            error: None,
            protocol: 20,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            agents: Vec::new(),
            socket_path: Some("/tmp/herdr.sock".into()),
            client_focused: Some(true),
        }
    }

    #[test]
    fn production_commands_have_exact_arguments() {
        let command = relay_command(std::ffi::OsStr::new("ssh"), &host());
        assert_eq!(command.get_program(), "ssh");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "ConnectionAttempts=1",
                "-o",
                "ServerAliveInterval=15",
                "ssh-target",
                "herdr-hub",
                "relay",
            ]
        );
        let command = version_command(std::ffi::OsStr::new("ssh"), &host());
        assert_eq!(command.get_program(), "ssh");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "ConnectionAttempts=1",
                "ssh-target",
                "herdr-hub",
                "--version",
            ]
        );
    }

    #[test]
    fn version_check_requires_exact_successful_output() {
        let directory = temp_directory("version");
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("fake-ssh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf 'herdr-hub {}\\n'\n",
                env!("CARGO_PKG_VERSION")
            ),
        )
        .unwrap();
        make_executable(&script);
        assert_eq!(
            check_version_with_program(&host(), script.as_os_str()).unwrap(),
            format!("herdr-hub {}", env!("CARGO_PKG_VERSION"))
        );

        std::fs::write(&script, "#!/bin/sh\nprintf 'herdr-hub wrong\\n'\n").unwrap();
        assert!(check_version_with_program(&host(), script.as_os_str()).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn version_check_timeout_kills_and_reaps_the_process_group() {
        let directory = temp_directory("version-timeout");
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("fake-ssh");
        let marker = directory.join("descendant-survived");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n(sleep 0.2; touch '{}') &\nexec sleep 30\n",
                marker.display()
            ),
        )
        .unwrap();
        make_executable(&script);

        let started = Instant::now();
        let error = check_version_with_program_and_timeout(
            &host(),
            script.as_os_str(),
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(300));
        assert!(
            !marker.exists(),
            "version-check descendant survived timeout"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn successful_version_check_kills_pipe_holding_descendants() {
        let directory = temp_directory("version-descendant");
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("fake-ssh");
        let pid_path = directory.join("descendant-pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 30 &\nprintf '%s\\n' \"$!\" > '{}'\nprintf 'herdr-hub {}\\n'\n",
                pid_path.display(),
                env!("CARGO_PKG_VERSION")
            ),
        )
        .unwrap();
        make_executable(&script);

        let started = Instant::now();
        assert_eq!(
            check_version_with_program_and_timeout(&host(), script.as_os_str(), TEST_TIMEOUT,)
                .unwrap(),
            format!("herdr-hub {}", env!("CARGO_PKG_VERSION"))
        );
        assert!(started.elapsed() < TEST_TIMEOUT);
        let descendant: i32 = std::fs::read_to_string(pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            // SAFETY: signal zero only checks existence of this test's descendant.
            if unsafe { libc::kill(descendant, 0) } == -1 {
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "version-check descendant survived successful leader exit"
            );
            thread::sleep(Duration::from_millis(10));
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn session_rewrite_is_strict_and_clears_remote_focus() {
        let (_, rewritten) =
            rewrite_session("workbox", session("local/default", "local", "default")).unwrap();
        assert_eq!(rewritten.key, "workbox/default");
        assert_eq!(rewritten.host, "workbox");
        assert_eq!(rewritten.socket_path, None);
        assert_eq!(rewritten.client_focused, None);

        assert!(rewrite_session("workbox", session("other/default", "other", "default")).is_none());
        assert!(rewrite_session("workbox", session("local/wrong", "local", "default")).is_none());
        assert!(rewrite_session("workbox", session("local/a/b", "local", "a/b")).is_none());
        assert_eq!(
            relay_session_key("workbox", "workbox/default").unwrap(),
            "local/default"
        );
        assert!(relay_session_key("workbox", "other/default").is_err());
        assert!(relay_session_key("workbox", "workbox/a/b").is_err());
    }

    #[test]
    fn backoff_caps_at_thirty_seconds() {
        let mut backoff = MIN_BACKOFF;
        let mut seconds = Vec::new();
        for _ in 0..7 {
            seconds.push(backoff.as_secs());
            backoff = next_backoff(backoff);
        }
        assert_eq!(seconds, [1, 2, 4, 8, 16, 30, 30]);
    }

    #[test]
    fn host_handle_drop_stops_and_joins_its_worker() {
        let (wake_rx, wake_tx) = UnixStream::pair().unwrap();
        wake_rx.set_nonblocking(true).unwrap();
        wake_tx.set_nonblocking(true).unwrap();
        let (commands, command_rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let exited = Arc::new(AtomicBool::new(false));
        let worker_exited = Arc::clone(&exited);
        let thread = thread::spawn(move || {
            let _commands = command_rx;
            wait_for_wake(&wake_rx, None).unwrap();
            assert!(worker_stop.load(Ordering::Acquire));
            worker_exited.store(true, Ordering::Release);
        });
        let handle = HostHandle {
            commands,
            wake: wake_tx,
            epoch: Arc::new(AtomicU64::new(0)),
            stop,
            thread: Some(thread),
        };

        drop(handle);
        assert!(exited.load(Ordering::Acquire));
    }

    #[test]
    fn supervisor_filters_stream_and_maps_call_reply() {
        let directory = temp_directory("transport");
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("fake-ssh");
        let args_path = directory.join("args");
        let call_path = directory.join("call");
        let hello = ServerMessage::Hello {
            protocol: PROTOCOL,
            model: Model {
                version: 4,
                active: Some("local/default".into()),
                hosts: vec![HostState {
                    key: "local".into(),
                    connected: true,
                    error: None,
                }],
                sessions: vec![
                    session("local/default", "local", "default"),
                    session("nested/hidden", "nested", "hidden"),
                ],
            },
        };
        let update = ServerMessage::Session {
            version: 5,
            session: session("local/agents", "local", "agents"),
        };
        let nested = ServerMessage::Session {
            version: 6,
            session: session("nested/ignored", "nested", "ignored"),
        };
        let removed = ServerMessage::SessionRemoved {
            version: 7,
            key: "local/agents".into(),
        };
        let reply = ServerMessage::Reply {
            id: 1,
            result: json!({"focused": true}),
        };
        let source = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf '%s\\n' '{}'\nIFS= read -r call\nprintf '%s\\n' \"$call\" > '{}'\nprintf '%s\\n' '{}'\nprintf '%s\\n' '{}'\nprintf '%s\\n' '{{\"type\":\"active\",\"version\":8,\"key\":null}}'\nprintf '%s\\n' '{}'\nprintf '%s\\n' '{}'\n",
            args_path.display(),
            serde_json::to_string(&hello).unwrap(),
            call_path.display(),
            serde_json::to_string(&update).unwrap(),
            serde_json::to_string(&nested).unwrap(),
            serde_json::to_string(&removed).unwrap(),
            serde_json::to_string(&reply).unwrap(),
        );
        std::fs::write(&script, source).unwrap();
        make_executable(&script);

        let (events_tx, events) = mpsc::sync_channel(32);
        let supervisor =
            Supervisor::spawn_with_program(vec![host()], events_tx, script.into_os_string())
                .unwrap();
        let connected = events.recv_timeout(TEST_TIMEOUT).unwrap();
        match connected {
            Event::Connected {
                host,
                epoch,
                sessions,
            } => {
                assert_eq!(host, "workbox");
                assert_eq!(epoch, 1);
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].key, "workbox/default");
                assert_eq!(sessions[0].socket_path, None);
                assert_eq!(sessions[0].client_focused, None);
            }
            other => panic!("expected Connected, got {other:?}"),
        }
        let (caller, _peer) = UnixStream::pair().unwrap();
        supervisor
            .call(
                "workbox",
                call(91, "pane.focus", json!({"pane_id": "w1:p1"}), &caller),
            )
            .unwrap();

        let mut saw_update = false;
        let mut saw_removed = false;
        let mut saw_reply = false;
        let mut saw_disconnect = false;
        for _ in 0..4 {
            match events.recv_timeout(TEST_TIMEOUT).unwrap() {
                Event::Session {
                    host,
                    epoch,
                    session,
                } => {
                    assert_eq!((host.as_str(), epoch), ("workbox", 1));
                    assert_eq!(session.key, "workbox/agents");
                    saw_update = true;
                }
                Event::SessionRemoved { host, epoch, key } => {
                    assert_eq!((host.as_str(), epoch), ("workbox", 1));
                    assert_eq!(key, "workbox/agents");
                    saw_removed = true;
                }
                Event::Reply {
                    host,
                    epoch,
                    token,
                    result,
                } => {
                    assert_eq!((host.as_str(), epoch, token), ("workbox", 1, 91));
                    assert_eq!(result.unwrap(), json!({"focused": true}));
                    saw_reply = true;
                }
                Event::Disconnected { host, epoch, .. } => {
                    assert_eq!((host.as_str(), epoch), ("workbox", 1));
                    saw_disconnect = true;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(saw_update && saw_removed && saw_reply && saw_disconnect);
        drop(supervisor);

        assert_eq!(
            std::fs::read_to_string(args_path).unwrap().trim(),
            "-o BatchMode=yes -o ConnectTimeout=5 -o ConnectionAttempts=1 -o ServerAliveInterval=15 ssh-target herdr-hub relay"
        );
        let call: ClientMessage =
            serde_json::from_str(&std::fs::read_to_string(call_path).unwrap()).unwrap();
        assert!(matches!(
            call,
            ClientMessage::Call { id, session, .. }
                if id == 1 && session == "local/default"
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn disconnect_fails_an_in_flight_call() {
        let directory = temp_directory("disconnect");
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("fake-ssh");
        let hello = ServerMessage::Hello {
            protocol: PROTOCOL,
            model: Model {
                version: 1,
                active: None,
                hosts: Vec::new(),
                sessions: vec![session("local/default", "local", "default")],
            },
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\nIFS= read -r call\n",
                serde_json::to_string(&hello).unwrap()
            ),
        )
        .unwrap();
        make_executable(&script);

        let (events_tx, events) = mpsc::sync_channel(16);
        let supervisor =
            Supervisor::spawn_with_program(vec![host()], events_tx, script.into_os_string())
                .unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            Event::Connected { epoch: 1, .. }
        ));
        let (caller, _peer) = UnixStream::pair().unwrap();
        supervisor
            .call("workbox", call(77, "pane.focus", json!({}), &caller))
            .unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            Event::Reply {
                epoch: 1,
                token: 77,
                result: Err(_),
                ..
            }
        ));
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            Event::Disconnected { epoch: 1, .. }
        ));
        drop(supervisor);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn shutdown_interrupts_a_backpressured_relay_write() {
        let directory = temp_directory("backpressure");
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("fake-ssh");
        let hello = ServerMessage::Hello {
            protocol: PROTOCOL,
            model: Model {
                version: 1,
                active: None,
                hosts: Vec::new(),
                sessions: vec![session("local/default", "local", "default")],
            },
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\nexec /bin/sleep 30\n",
                serde_json::to_string(&hello).unwrap()
            ),
        )
        .unwrap();
        make_executable(&script);

        let (events_tx, events) = mpsc::sync_channel(16);
        let supervisor =
            Supervisor::spawn_with_program(vec![host()], events_tx, script.into_os_string())
                .unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            Event::Connected { epoch: 1, .. }
        ));

        let params = Value::String("x".repeat(256 * 1024));
        let (caller, _peer) = UnixStream::pair().unwrap();
        let mut queue_filled = false;
        for token in 1..=100 {
            if supervisor
                .call(
                    "workbox",
                    call(token, "pane.focus", params.clone(), &caller),
                )
                .is_err()
            {
                queue_filled = true;
                break;
            }
        }
        assert!(queue_filled, "relay writer never became backpressured");

        let started = Instant::now();
        drop(supervisor);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "backpressured supervisor shutdown took {:?}",
            started.elapsed()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unanswered_calls_remain_bounded() {
        let directory = temp_directory("unanswered");
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("fake-ssh");
        let hello = ServerMessage::Hello {
            protocol: PROTOCOL,
            model: Model {
                version: 1,
                active: None,
                hosts: Vec::new(),
                sessions: vec![session("local/default", "local", "default")],
            },
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\nwhile IFS= read -r call; do :; done\n",
                serde_json::to_string(&hello).unwrap()
            ),
        )
        .unwrap();
        make_executable(&script);

        let (events_tx, events) = mpsc::sync_channel(16);
        let supervisor =
            Supervisor::spawn_with_program(vec![host()], events_tx, script.into_os_string())
                .unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            Event::Connected { epoch: 1, .. }
        ));

        let mut accepted = 0;
        let mut rejected = 0;
        let (caller, _peer) = UnixStream::pair().unwrap();
        for token in 1..=(MAX_IN_FLIGHT + COMMAND_QUEUE + 64) as u64 {
            match supervisor.call("workbox", call(token, "pane.focus", json!({}), &caller)) {
                Ok(()) => accepted += 1,
                Err(_) => rejected += 1,
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            accepted <= MAX_IN_FLIGHT + COMMAND_QUEUE,
            "accepted {accepted}"
        );
        assert!(rejected > 0);

        drop(supervisor);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reply_burst_skips_abandoned_calls_and_releases_live_calls_without_an_extra_wake() {
        let directory = temp_directory("reply-burst");
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("fake-ssh");
        let release = directory.join("release");
        let forwarded = directory.join("forwarded");
        let hello = ServerMessage::Hello {
            protocol: PROTOCOL,
            model: Model {
                version: 1,
                active: None,
                hosts: Vec::new(),
                sessions: vec![session("local/default", "local", "default")],
            },
        };
        let replies = (1..=MAX_IN_FLIGHT as u64)
            .map(|counter| {
                serde_json::to_string(&ServerMessage::Reply {
                    id: counter,
                    result: json!({"counter": counter}),
                })
                .unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let abandoned = MAX_IN_FLIGHT as u64 + 1;
        let final_counter = abandoned + 1;
        let final_reply = serde_json::to_string(&ServerMessage::Reply {
            id: final_counter,
            result: json!({"counter": final_counter}),
        })
        .unwrap();
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\ni=0\nwhile [ \"$i\" -lt {} ]; do\n  IFS= read -r call || exit 1\n  i=$((i + 1))\ndone\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nprintf '%s\\n' '{}'\nIFS= read -r final || exit 1\nprintf '%s\\n' \"$final\" > '{}'\nprintf '%s\\n' '{}'\n",
                serde_json::to_string(&hello).unwrap(),
                MAX_IN_FLIGHT,
                release.display(),
                replies,
                forwarded.display(),
                final_reply,
            ),
        )
        .unwrap();
        make_executable(&script);

        let (events_tx, events) = mpsc::sync_channel(128);
        let supervisor =
            Supervisor::spawn_with_program(vec![host()], events_tx, script.into_os_string())
                .unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            Event::Connected { epoch: 1, .. }
        ));

        let (caller, _peer) = UnixStream::pair().unwrap();
        let (expired_caller, mut expired_peer) = UnixStream::pair().unwrap();
        let queue_deadline = Instant::now() + TEST_TIMEOUT;
        for token in 1..=final_counter {
            loop {
                let (method, connection) = if token == abandoned {
                    ("pane.focus", &expired_caller)
                } else {
                    ("test", &caller)
                };
                match supervisor.call("workbox", call(token, method, json!({}), connection)) {
                    Ok(()) => break,
                    Err(error) if error.to_string().contains("call queue is full") => {
                        assert!(Instant::now() < queue_deadline, "call queue remained full");
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("cannot queue call {token}: {error:#}"),
                }
            }
        }

        // A timed-out SDK call drops its dedicated socket. Hold all 64 slots until
        // that happens, so the focus request is definitely abandoned while queued.
        expired_peer
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let error = expired_peer.read(&mut [0]).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        drop(expired_peer);
        std::fs::write(&release, "").unwrap();

        let mut replied = std::collections::HashSet::new();
        while replied.len() < final_counter as usize {
            match events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Reply { token, result, .. } => {
                    if token == abandoned {
                        assert!(result.unwrap_err().contains("caller disconnected"));
                    } else {
                        assert_eq!(result.unwrap(), json!({"counter": token}));
                    }
                    replied.insert(token);
                }
                Event::Disconnected { error, .. } => {
                    panic!("relay disconnected before all replies: {error}")
                }
                _ => {}
            }
        }
        assert!(replied.contains(&final_counter));
        let final_call: ClientMessage =
            serde_json::from_str(&std::fs::read_to_string(forwarded).unwrap()).unwrap();
        assert!(
            matches!(final_call, ClientMessage::Call { id, method, .. } if id == final_counter && method == "test")
        );

        drop(supervisor);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn backpressured_write_drains_relay_output_and_recovers() {
        let directory = temp_directory("full-duplex");
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("fake-ssh");
        let hello = ServerMessage::Hello {
            protocol: PROTOCOL,
            model: Model {
                version: 1,
                active: None,
                hosts: Vec::new(),
                sessions: vec![session("local/default", "local", "default")],
            },
        };
        let marker = ServerMessage::Session {
            version: 2,
            session: session("local/marker", "local", "marker"),
        };
        let reply_one = ServerMessage::Reply {
            id: 1,
            result: json!({"call": 1}),
        };
        let reply_two = ServerMessage::Reply {
            id: 2,
            result: json!({"call": 2}),
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\nIFS= read -r first\nprintf '%s\\n' '{}'\ni=0\nwhile [ \"$i\" -lt 10000 ]; do\n  printf '%s\\n' '{{\"type\":\"active\",\"version\":3,\"key\":null}}'\n  i=$((i + 1))\ndone\nIFS= read -r second\nprintf '%s\\n' '{}'\nprintf '%s\\n' '{}'\n",
                serde_json::to_string(&hello).unwrap(),
                serde_json::to_string(&marker).unwrap(),
                serde_json::to_string(&reply_one).unwrap(),
                serde_json::to_string(&reply_two).unwrap(),
            ),
        )
        .unwrap();
        make_executable(&script);

        let (events_tx, events) = mpsc::sync_channel(32);
        let supervisor =
            Supervisor::spawn_with_program(vec![host()], events_tx, script.into_os_string())
                .unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            Event::Connected { epoch: 1, .. }
        ));
        let (caller, _peer) = UnixStream::pair().unwrap();
        supervisor
            .call("workbox", call(41, "first", json!({}), &caller))
            .unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            Event::Session { session, .. } if session.key == "workbox/marker"
        ));
        supervisor
            .call(
                "workbox",
                call(42, "second", Value::String("x".repeat(256 * 1024)), &caller),
            )
            .unwrap();

        let mut replies = HashMap::new();
        while replies.len() < 2 {
            match events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Reply { token, result, .. } => {
                    replies.insert(token, result.unwrap());
                }
                Event::Disconnected { error, .. } => {
                    panic!("relay disconnected before both replies: {error}")
                }
                _ => {}
            }
        }
        assert_eq!(replies[&41], json!({"call": 1}));
        assert_eq!(replies[&42], json!({"call": 2}));
        drop(supervisor);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn temp_directory(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "herdr-hub-remote-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).unwrap();
    }
}
