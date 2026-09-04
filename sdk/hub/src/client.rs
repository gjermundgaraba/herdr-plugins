use std::{
    fmt,
    io::{self, BufReader, Write},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd},
        unix::net::UnixStream,
    },
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::Value;

use crate::{ClientMessage, Model, PROTOCOL, ServerMessage, socket_path};

const MIN_RECONNECT_BACKOFF: Duration = Duration::from_millis(250);
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_json::Error),
    Protocol { expected: u32, actual: u32 },
    Server(String),
    UnexpectedMessage(&'static str),
    MismatchedReply { expected: u64, actual: u64 },
    Disconnected,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
            Self::Protocol { expected, actual } => {
                write!(
                    f,
                    "hub protocol {actual} does not match client protocol {expected}"
                )
            }
            Self::Server(error) => write!(f, "{error}"),
            Self::UnexpectedMessage(expected) => {
                write!(f, "unexpected hub message; expected {expected}")
            }
            Self::MismatchedReply { expected, actual } => {
                write!(
                    f,
                    "hub reply id {actual} does not match request id {expected}"
                )
            }
            Self::Disconnected => f.write_str("hub disconnected"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Clone)]
pub struct HubClient {
    socket_path: PathBuf,
}

impl Default for HubClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HubClient {
    pub fn new() -> Self {
        Self {
            socket_path: socket_path(),
        }
    }

    pub fn subscribe(&self, timeout: Duration) -> Result<(Stream, Model)> {
        self.subscribe_at(timeout)
    }

    pub fn call(
        &self,
        session: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut socket = self.connect(timeout)?;
        write_message(
            &mut socket,
            &ClientMessage::Call {
                protocol: PROTOCOL,
                id,
                session: session.into(),
                method: method.into(),
                params,
            },
        )?;
        let mut reader = BufReader::new(socket);
        let mut pending = Vec::new();
        match read_message_with_timeout(&mut reader, &mut pending, timeout)? {
            Some(ServerMessage::Reply {
                id: reply_id,
                result,
            }) if reply_id == id => Ok(result),
            Some(ServerMessage::ReplyError {
                id: reply_id,
                error,
            }) if reply_id == id => Err(Error::Server(error)),
            Some(ServerMessage::Reply { id: actual, .. })
            | Some(ServerMessage::ReplyError { id: actual, .. }) => Err(Error::MismatchedReply {
                expected: id,
                actual,
            }),
            Some(ServerMessage::Error { error }) => Err(Error::Server(error)),
            Some(_) => Err(Error::UnexpectedMessage("reply")),
            None => Err(Error::Disconnected),
        }
    }

    pub fn apply(model: &mut Model, message: &ServerMessage) {
        match message {
            ServerMessage::Hello {
                protocol: _,
                model: replacement,
            } => *model = replacement.clone(),
            ServerMessage::Session { version, session } => {
                if let Some(current) = model
                    .sessions
                    .iter_mut()
                    .find(|current| current.key == session.key)
                {
                    *current = session.clone();
                } else {
                    model.sessions.push(session.clone());
                }
                model.version = *version;
            }
            ServerMessage::SessionRemoved { version, key } => {
                model.sessions.retain(|session| session.key != *key);
                model.version = *version;
            }
            ServerMessage::Host { version, host } => {
                if let Some(current) = model
                    .hosts
                    .iter_mut()
                    .find(|current| current.key == host.key)
                {
                    *current = host.clone();
                } else {
                    model.hosts.push(host.clone());
                }
                model.version = *version;
            }
            ServerMessage::Active { version, key } => {
                model.active.clone_from(key);
                model.version = *version;
            }
            ServerMessage::Reply { .. }
            | ServerMessage::ReplyError { .. }
            | ServerMessage::Error { .. } => {}
        }
    }

    pub fn run(&self, mut on_message: impl FnMut(Result<ServerMessage>)) -> ! {
        let mut backoff = MIN_RECONNECT_BACKOFF;
        let mut outage_reported = false;
        loop {
            let error = match self.subscribe(CONNECT_TIMEOUT) {
                Ok((mut stream, model)) => {
                    backoff = MIN_RECONNECT_BACKOFF;
                    outage_reported = false;
                    on_message(Ok(ServerMessage::Hello {
                        protocol: PROTOCOL,
                        model,
                    }));
                    loop {
                        match stream.next() {
                            Ok(Some(message)) => on_message(Ok(message)),
                            Ok(None) => break Error::Disconnected,
                            Err(error) => break error,
                        }
                    }
                }
                Err(error) => error,
            };
            if !outage_reported {
                on_message(Err(error));
                outage_reported = true;
            }
            thread::sleep(backoff);
            backoff = backoff.saturating_mul(2).min(MAX_RECONNECT_BACKOFF);
        }
    }

    fn subscribe_at(&self, timeout: Duration) -> Result<(Stream, Model)> {
        let mut socket = self.connect(timeout)?;
        write_message(
            &mut socket,
            &ClientMessage::Subscribe { protocol: PROTOCOL },
        )?;
        let mut reader = BufReader::new(socket);
        let mut pending = Vec::new();
        let model = match read_message_with_timeout(&mut reader, &mut pending, timeout)? {
            Some(ServerMessage::Hello { protocol, model }) if protocol == PROTOCOL => model,
            Some(ServerMessage::Hello { protocol, .. }) => {
                return Err(Error::Protocol {
                    expected: PROTOCOL,
                    actual: protocol,
                });
            }
            Some(ServerMessage::Error { error }) => return Err(Error::Server(error)),
            Some(_) => return Err(Error::UnexpectedMessage("hello")),
            None => return Err(Error::Disconnected),
        };
        Ok((
            Stream {
                reader,
                pending,
                ended: false,
            },
            model,
        ))
    }

    fn connect(&self, timeout: Duration) -> Result<UnixStream> {
        let stream = UnixStream::connect(&self.socket_path)?;
        herdr_client::unix::peer_is_current_user(&stream)?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(stream)
    }

    #[cfg(test)]
    fn at(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }
}

pub struct Stream {
    reader: BufReader<UnixStream>,
    pending: Vec<u8>,
    ended: bool,
}

impl Stream {
    #[allow(clippy::should_implement_trait)] // The hub protocol names this blocking operation `next`.
    pub fn next(&mut self) -> Result<Option<ServerMessage>> {
        if self.ended {
            return Ok(None);
        }
        match read_message(&mut self.reader, &mut self.pending) {
            Ok(Some(message)) => Ok(Some(message)),
            Ok(None) => {
                self.ended = true;
                Ok(None)
            }
            Err(Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData
                ) =>
            {
                self.ended = true;
                Err(Error::Io(error))
            }
            Err(error) => Err(error),
        }
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.reader.get_ref().as_fd()
    }
}

fn write_message(stream: &mut UnixStream, message: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *stream, message)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn read_message(
    reader: &mut BufReader<UnixStream>,
    pending: &mut Vec<u8>,
) -> Result<Option<ServerMessage>> {
    herdr_client::ndjson::read_frame(reader, pending)?
        .map(|frame| serde_json::from_slice(&frame).map_err(Error::Json))
        .transpose()
}

fn read_message_with_timeout(
    reader: &mut BufReader<UnixStream>,
    pending: &mut Vec<u8>,
    timeout: Duration,
) -> Result<Option<ServerMessage>> {
    reader.get_ref().set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    let result = loop {
        match read_message(reader, pending) {
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break Err(Error::Io(io::ErrorKind::TimedOut.into()));
                }
                let mut descriptor = libc::pollfd {
                    fd: reader.get_ref().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let milliseconds = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
                // SAFETY: descriptor points to one initialized pollfd for this call.
                let ready = unsafe { libc::poll(&raw mut descriptor, 1, milliseconds) };
                if ready == 0 {
                    break Err(Error::Io(io::ErrorKind::TimedOut.into()));
                }
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        break Err(Error::Io(error));
                    }
                }
            }
            result => break result,
        }
    };
    reader.get_ref().set_nonblocking(false)?;
    result
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        os::fd::AsRawFd,
        os::unix::net::UnixListener,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::{HostState, SessionState};

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

    fn test_socket(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-hub-client-{name}-{}-{}.sock",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ))
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

    fn session(key: &str, connected: bool) -> SessionState {
        SessionState {
            key: key.into(),
            host: "local".into(),
            name: key.rsplit('/').next().unwrap().into(),
            connected,
            error: None,
            protocol: 20,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            agents: Vec::new(),
            socket_path: Some("/tmp/herdr.sock".into()),
            client_focused: None,
        }
    }

    #[test]
    fn subscribe_requires_hello_and_streams_updates() {
        let path = test_socket("subscribe");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(socket.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<ClientMessage>(&request).unwrap(),
                ClientMessage::Subscribe { protocol: PROTOCOL }
            );
            writeln!(
                socket,
                "{}",
                serde_json::to_string(&ServerMessage::Hello {
                    protocol: PROTOCOL,
                    model: model(3),
                })
                .unwrap()
            )
            .unwrap();
            writeln!(
                socket,
                "{}",
                serde_json::to_string(&ServerMessage::Active {
                    version: 4,
                    key: Some("local/default".into()),
                })
                .unwrap()
            )
            .unwrap();
        });

        let (mut stream, initial) = HubClient::at(&path)
            .subscribe(Duration::from_secs(1))
            .unwrap();
        assert_eq!(initial.version, 3);
        assert!(stream.as_fd().as_raw_fd() >= 0);
        assert_eq!(
            stream.next().unwrap(),
            Some(ServerMessage::Active {
                version: 4,
                key: Some("local/default".into()),
            })
        );
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn call_uses_a_dedicated_one_reply_connection() {
        let path = test_socket("call");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(socket.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request = serde_json::from_str::<ClientMessage>(&request).unwrap();
            let ClientMessage::Call {
                id,
                session,
                method,
                params,
                ..
            } = request
            else {
                panic!("expected call")
            };
            assert_eq!(session, "local/default");
            assert_eq!(method, "pane.focus");
            assert_eq!(params["pane_id"], "w1:p1");
            writeln!(
                socket,
                "{}",
                serde_json::to_string(&ServerMessage::Reply {
                    id,
                    result: serde_json::json!({ "focused": true }),
                })
                .unwrap()
            )
            .unwrap();
        });

        let result = HubClient::at(&path)
            .call(
                "local/default",
                "pane.focus",
                serde_json::json!({ "pane_id": "w1:p1" }),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(result, serde_json::json!({ "focused": true }));
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn apply_replaces_and_removes_by_key_and_sets_active() {
        let mut current = model(0);
        current.sessions.push(session("local/default", false));

        HubClient::apply(
            &mut current,
            &ServerMessage::Session {
                version: 1,
                session: session("local/default", true),
            },
        );
        assert_eq!(current.sessions.len(), 1);
        assert!(current.sessions[0].connected);

        HubClient::apply(
            &mut current,
            &ServerMessage::Host {
                version: 2,
                host: HostState {
                    key: "local".into(),
                    connected: false,
                    error: Some("offline".into()),
                },
            },
        );
        assert_eq!(current.hosts.len(), 1);
        assert_eq!(current.hosts[0].error.as_deref(), Some("offline"));

        HubClient::apply(
            &mut current,
            &ServerMessage::Active {
                version: 3,
                key: Some("local/default".into()),
            },
        );
        assert_eq!(current.active.as_deref(), Some("local/default"));

        HubClient::apply(
            &mut current,
            &ServerMessage::SessionRemoved {
                version: 4,
                key: "local/default".into(),
            },
        );
        assert!(current.sessions.is_empty());
        assert_eq!(current.version, 4);
    }
}
