use std::{
    collections::HashMap,
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Write},
    net::Shutdown,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use herdr_client::{
    ndjson, open_rotating_log,
    unix::{bind_private_socket, peer_is_current_user},
};
use herdr_hub_client::{ClientMessage, Model, PROTOCOL, ServerMessage};
use serde::Serialize;
use serde_json::Value;

use crate::hub::Event;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const SUBSCRIBER_QUEUE: usize = 64;

pub(crate) fn log_error(message: &str) {
    eprintln!("herdr-hub: {message}");
    let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) else {
        return;
    };
    let directory = PathBuf::from(home).join("Library/Logs/herdr-hub");
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    if let Ok(mut file) = open_rotating_log(&directory.join("hub.log"), 10 << 20, 3) {
        let _ = writeln!(file, "{message}");
    }
}

pub(crate) enum Request {
    Subscribe {
        id: u64,
        stream: UnixStream,
    },
    Unsubscribe {
        id: u64,
    },
    Call {
        stream: UnixStream,
        id: u64,
        session: String,
        method: String,
        params: Value,
    },
    Notify {
        socket_path: PathBuf,
        event: Option<Value>,
    },
}

struct Subscriber {
    outgoing: SyncSender<ServerMessage>,
    shutdown: UnixStream,
}

pub(crate) struct Server {
    subscribers: HashMap<u64, Subscriber>,
    stop: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
    socket_path: PathBuf,
    socket_identity: SocketIdentity,
    _lock: File,
}

impl Server {
    pub(crate) fn start(tx: SyncSender<Event>) -> Result<Self> {
        let uid = effective_uid();
        if uid == 0 {
            bail!("Herdr Hub refuses to run as root")
        }
        let socket_path = herdr_hub_client::socket_path();
        let lock_path = PathBuf::from(format!("/tmp/herdr-hub-{uid}.lock"));
        Self::start_at(tx, socket_path, lock_path)
    }

    fn start_at(tx: SyncSender<Event>, socket_path: PathBuf, lock_path: PathBuf) -> Result<Self> {
        let lock = acquire_lock(&lock_path)?;
        let listener = bind_socket(&socket_path)?;
        listener.set_nonblocking(true)?;
        let socket_identity = socket_identity(&socket_path)?
            .ok_or_else(|| anyhow!("hub socket disappeared after bind"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let next_subscriber = Arc::new(AtomicU64::new(1));
        let accept_thread = thread::Builder::new()
            .name("herdr-hub-accept".into())
            .spawn(move || accept_loop(listener, tx, thread_stop, next_subscriber))?;
        Ok(Self {
            subscribers: HashMap::new(),
            stop,
            accept_thread: Some(accept_thread),
            socket_path,
            socket_identity,
            _lock: lock,
        })
    }

    pub(crate) fn add_subscriber(
        &mut self,
        id: u64,
        stream: UnixStream,
        model: &Model,
    ) -> Result<()> {
        let shutdown = stream.try_clone()?;
        let (outgoing, receiver) = mpsc::sync_channel(SUBSCRIBER_QUEUE);
        let hello = ServerMessage::Hello {
            protocol: PROTOCOL,
            model: model.clone(),
        };
        thread::Builder::new()
            .name("herdr-hub-subscriber".into())
            .spawn(move || subscriber_writer(stream, hello, receiver))?;
        self.subscribers
            .insert(id, Subscriber { outgoing, shutdown });
        Ok(())
    }

    pub(crate) fn remove_subscriber(&mut self, id: u64) {
        if let Some(subscriber) = self.subscribers.remove(&id) {
            let _ = subscriber.shutdown.shutdown(Shutdown::Both);
        }
    }

    pub(crate) fn broadcast(&mut self, message: &ServerMessage) {
        self.subscribers.retain(|_, subscriber| {
            match subscriber.outgoing.try_send(message.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    let _ = subscriber.shutdown.shutdown(Shutdown::Both);
                    false
                }
            }
        });
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for subscriber in self.subscribers.values() {
            let _ = subscriber.shutdown.shutdown(Shutdown::Both);
        }
        if let Some(thread) = self.accept_thread.take() {
            let _ = thread.join();
        }
        if socket_identity(&self.socket_path).ok().flatten() == Some(self.socket_identity) {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

pub(crate) fn reply_call(
    mut stream: UnixStream,
    id: u64,
    reply: std::result::Result<Value, String>,
) {
    let message = match reply {
        Ok(result) => ServerMessage::Reply { id, result },
        Err(error) => ServerMessage::ReplyError { id, error },
    };
    let _ = write_message(&mut stream, &message);
    let _ = stream.shutdown(Shutdown::Both);
}

fn accept_loop(
    listener: UnixListener,
    tx: SyncSender<Event>,
    stop: Arc<AtomicBool>,
    next_subscriber: Arc<AtomicU64>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if peer_is_current_user(&stream).is_err() {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let tx = tx.clone();
                let id = next_subscriber.fetch_add(1, Ordering::Relaxed);
                let _ = thread::Builder::new()
                    .name("herdr-hub-connection".into())
                    .spawn(move || {
                        let _ = read_connection(stream, id, tx);
                    });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn read_connection(
    mut stream: UnixStream,
    connection_id: u64,
    tx: SyncSender<Event>,
) -> Result<()> {
    stream.set_nonblocking(true)?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let first = match read_handshake::<ClientMessage>(&mut reader, HANDSHAKE_TIMEOUT) {
        Ok(message) => message,
        Err(error) => {
            let _ = write_message(
                &mut stream,
                &ServerMessage::Error {
                    error: error.to_string(),
                },
            );
            return Err(error);
        }
    };
    // Duplicated Unix stream descriptors share O_NONBLOCK, so changing the
    // accepted stream also restores the reader clone to blocking mode.
    stream.set_nonblocking(false)?;
    match first {
        ClientMessage::Subscribe { protocol } if protocol == PROTOCOL => {
            tx.send(Event::Server(Request::Subscribe {
                id: connection_id,
                stream: stream.try_clone()?,
            }))
            .map_err(|_| anyhow!("hub stopped"))?;
            let mut pending = Vec::new();
            let result = match ndjson::read_frame(&mut reader, &mut pending) {
                Ok(None) => Ok(()),
                _ => Err(anyhow!("subscriber sent a second message")),
            };
            let _ = tx.send(Event::Server(Request::Unsubscribe { id: connection_id }));
            let _ = stream.shutdown(Shutdown::Both);
            result
        }
        ClientMessage::Call {
            protocol,
            id,
            session,
            method,
            params,
        } if protocol == PROTOCOL => tx
            .send(Event::Server(Request::Call {
                stream,
                id,
                session,
                method,
                params,
            }))
            .map_err(|_| anyhow!("hub stopped")),
        ClientMessage::Notify {
            protocol,
            socket_path,
            event,
        } if protocol == PROTOCOL => tx
            .send(Event::Server(Request::Notify { socket_path, event }))
            .map_err(|_| anyhow!("hub stopped")),
        _ => {
            let error = "incompatible Herdr Hub protocol";
            let _ = write_message(
                &mut stream,
                &ServerMessage::Error {
                    error: error.into(),
                },
            );
            bail!(error)
        }
    }
}

fn subscriber_writer(
    mut stream: UnixStream,
    hello: ServerMessage,
    outgoing: mpsc::Receiver<ServerMessage>,
) {
    if write_message(&mut stream, &hello).is_err() {
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }
    while let Ok(message) = outgoing.recv() {
        if write_message(&mut stream, &message).is_err() {
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
    }
}

fn read_handshake<T: serde::de::DeserializeOwned>(
    reader: &mut BufReader<UnixStream>,
    timeout: Duration,
) -> Result<T> {
    let deadline = Instant::now() + timeout;
    let mut pending = Vec::new();
    loop {
        match ndjson::read_frame(reader, &mut pending) {
            Ok(Some(frame)) => {
                return serde_json::from_slice(&frame).context("parse Herdr Hub message");
            }
            Ok(None) => bail!("Herdr Hub connection closed"),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error).context("read Herdr Hub message"),
        }
        let now = Instant::now();
        if now >= deadline {
            bail!("Herdr Hub handshake timed out")
        }
        let remaining = deadline.saturating_duration_since(now);
        let timeout_ms = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
        let mut descriptor = libc::pollfd {
            fd: reader.get_ref().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialized pollfd for this call.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error()).context("wait for Herdr Hub handshake");
        }
    }
}

fn write_message(stream: &mut impl Write, message: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec(message)?;
    if bytes.len() + 1 > ndjson::MAX_FRAME_BYTES {
        bail!("Herdr Hub message exceeds 1 MiB")
    }
    bytes.push(b'\n');
    stream.write_all(&bytes).context("write Herdr Hub message")
}

fn bind_socket(path: &Path) -> Result<UnixListener> {
    match bind_private_socket(path) {
        Ok(listener) => finish_bound_socket(path, listener),
        Err(error) if error.raw_os_error() == Some(libc::EADDRINUSE) => {
            let before = socket_identity(path)?;
            let metadata = fs::symlink_metadata(path)
                .with_context(|| format!("inspect existing hub socket {}", path.display()))?;
            if !metadata.file_type().is_socket() || metadata.uid() != effective_uid() {
                bail!("unsafe existing hub socket {}", path.display())
            }
            match UnixStream::connect(path) {
                Ok(_) => bail!("Herdr Hub is already running"),
                Err(error) if error.raw_os_error() == Some(libc::ECONNREFUSED) => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("connect existing hub socket {}", path.display())
                    });
                }
            }
            if before.is_none() || socket_identity(path)? != before {
                bail!("hub socket changed during startup")
            }
            fs::remove_file(path)
                .with_context(|| format!("remove stale hub socket {}", path.display()))?;
            let listener = bind_private_socket(path)
                .with_context(|| format!("bind hub socket {}", path.display()))?;
            finish_bound_socket(path, listener)
        }
        Err(error) => Err(error).with_context(|| format!("bind hub socket {}", path.display())),
    }
}

fn finish_bound_socket(path: &Path, listener: UnixListener) -> Result<UnixListener> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod hub socket {}", path.display()))?;
    Ok(listener)
}

fn acquire_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("open hub lock {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != effective_uid()
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
    {
        bail!("unsafe hub lock {}", path.display())
    }
    // SAFETY: file owns a live descriptor and LOCK_NB prevents blocking.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            bail!("Herdr Hub is already running")
        }
        return Err(error).context("lock Herdr Hub");
    }
    Ok(file)
}

fn effective_uid() -> libc::uid_t {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

fn socket_identity(path: &Path) -> Result<Option<SocketIdentity>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(SocketIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::BufReader,
        sync::atomic::AtomicUsize,
        time::{SystemTime, UNIX_EPOCH},
    };

    use herdr_hub_client::HostState;
    use serde_json::json;

    use super::*;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn paths(name: &str) -> (PathBuf, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let stem = format!(
            "herdr-hub-{name}-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let directory = std::env::temp_dir();
        (
            directory.join(format!("{stem}.sock")),
            directory.join(format!("{stem}.lock")),
        )
    }

    fn model(version: u64) -> Model {
        Model {
            version,
            active: None,
            hosts: vec![HostState {
                key: "local".into(),
                connected: true,
                error: None,
            }],
            sessions: Vec::new(),
        }
    }

    fn connect(path: &Path, message: &ClientMessage) -> UnixStream {
        let mut stream = UnixStream::connect(path).unwrap();
        write_message(&mut stream, message).unwrap();
        stream
    }

    fn next(reader: &mut BufReader<UnixStream>) -> Option<ServerMessage> {
        let mut pending = Vec::new();
        ndjson::read_frame(reader, &mut pending)
            .unwrap()
            .map(|frame| serde_json::from_slice(&frame).unwrap())
    }

    fn server(name: &str) -> (Server, mpsc::Receiver<Event>, PathBuf, PathBuf) {
        let (socket, lock) = paths(name);
        let (tx, rx) = mpsc::sync_channel(64);
        let server = Server::start_at(tx, socket.clone(), lock.clone()).unwrap();
        (server, rx, socket, lock)
    }

    fn cleanup(server: Server, lock: PathBuf) {
        drop(server);
        fs::remove_file(lock).unwrap();
    }

    #[test]
    fn subscribe_gets_atomic_hello_then_versioned_updates() {
        let (mut server, rx, socket, lock) = server("subscribe");
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let client = connect(&socket, &ClientMessage::Subscribe { protocol: PROTOCOL });
        let Event::Server(Request::Subscribe { id, stream }) =
            rx.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("expected subscribe request")
        };
        server.add_subscriber(id, stream, &model(7)).unwrap();
        let mut reader = BufReader::new(client);
        assert!(matches!(
            next(&mut reader),
            Some(ServerMessage::Hello { model, .. }) if model.version == 7
        ));
        server.broadcast(&ServerMessage::Active {
            version: 8,
            key: Some("local/default".into()),
        });
        server.broadcast(&ServerMessage::Host {
            version: 9,
            host: HostState {
                key: "local".into(),
                connected: true,
                error: None,
            },
        });
        assert!(matches!(
            next(&mut reader),
            Some(ServerMessage::Active { version: 8, .. })
        ));
        assert!(matches!(
            next(&mut reader),
            Some(ServerMessage::Host { version: 9, .. })
        ));
        cleanup(server, lock);
    }

    #[test]
    fn call_connection_receives_only_its_reply() {
        let (mut server, rx, socket, lock) = server("call");
        let client = connect(
            &socket,
            &ClientMessage::Call {
                protocol: PROTOCOL,
                id: 41,
                session: "local/default".into(),
                method: "pane.focus".into(),
                params: json!({"pane_id": "w1:p1"}),
            },
        );
        let Event::Server(Request::Call { stream, id, .. }) =
            rx.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("expected call request")
        };
        server.broadcast(&ServerMessage::Active {
            version: 1,
            key: None,
        });
        reply_call(stream, id, Ok(json!({"focused": true})));
        let mut reader = BufReader::new(client);
        assert!(matches!(
            next(&mut reader),
            Some(ServerMessage::Reply { id: 41, .. })
        ));
        assert!(next(&mut reader).is_none());
        cleanup(server, lock);
    }

    #[test]
    fn a_second_subscriber_message_closes_the_fixed_role() {
        let (mut server, rx, socket, lock) = server("role");
        let mut client = connect(&socket, &ClientMessage::Subscribe { protocol: PROTOCOL });
        let Event::Server(Request::Subscribe { id, stream }) =
            rx.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("expected subscribe request")
        };
        server.add_subscriber(id, stream, &model(0)).unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        assert!(matches!(
            next(&mut reader),
            Some(ServerMessage::Hello { .. })
        ));
        write_message(
            &mut client,
            &ClientMessage::Subscribe { protocol: PROTOCOL },
        )
        .unwrap();
        assert!(next(&mut reader).is_none());
        cleanup(server, lock);
    }

    #[test]
    fn subscriber_eof_removes_it_without_waiting_for_a_broadcast() {
        let (mut server, rx, socket, lock) = server("eof");
        let client = connect(&socket, &ClientMessage::Subscribe { protocol: PROTOCOL });
        let Event::Server(Request::Subscribe { id, stream }) =
            rx.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("expected subscribe request")
        };
        server.add_subscriber(id, stream, &model(0)).unwrap();
        let mut reader = BufReader::new(client);
        assert!(matches!(
            next(&mut reader),
            Some(ServerMessage::Hello { .. })
        ));
        drop(reader);

        let Event::Server(Request::Unsubscribe { id: closed }) =
            rx.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("expected unsubscribe request")
        };
        assert_eq!(closed, id);
        server.remove_subscriber(closed);
        assert!(server.subscribers.is_empty());
        cleanup(server, lock);
    }

    #[test]
    fn full_subscriber_queue_disconnects_instead_of_dropping_an_update() {
        let (mut server, _rx, _socket, lock) = server("slow");
        let (outgoing, _receiver) = mpsc::sync_channel(1);
        outgoing
            .send(ServerMessage::Active {
                version: 1,
                key: None,
            })
            .unwrap();
        let (shutdown, _peer) = UnixStream::pair().unwrap();
        server
            .subscribers
            .insert(1, Subscriber { outgoing, shutdown });
        server.broadcast(&ServerMessage::Active {
            version: 2,
            key: None,
        });
        assert!(server.subscribers.is_empty());
        cleanup(server, lock);
    }

    #[test]
    fn notify_is_forwarded_without_a_reply() {
        let (server, rx, socket, lock) = server("notify");
        let client = connect(
            &socket,
            &ClientMessage::Notify {
                protocol: PROTOCOL,
                socket_path: "/tmp/herdr/herdr.sock".into(),
                event: Some(json!({"event": "pane_agent_status_changed"})),
            },
        );
        let Event::Server(Request::Notify { socket_path, event }) =
            rx.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("expected notify request")
        };
        assert_eq!(socket_path, Path::new("/tmp/herdr/herdr.sock"));
        assert!(event.is_some());
        let mut reader = BufReader::new(client);
        assert!(next(&mut reader).is_none());
        cleanup(server, lock);
    }
}
