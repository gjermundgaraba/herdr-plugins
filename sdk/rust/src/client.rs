use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    AgentInfo, EventEnvelope, EventSubscription, PaneInfo, PingResult, SessionSnapshot, TabInfo,
    WorkspaceInfo,
};

type LocalStream = interprocess::local_socket::Stream;

#[derive(Debug, Clone)]
pub struct Client {
    socket_path: PathBuf,
    timeout: Option<Duration>,
    next_id: Arc<AtomicU64>,
}

impl Client {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout: None,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn from_env() -> Result<Self, Error> {
        std::env::var_os("HERDR_SOCKET_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(Self::new)
            .ok_or(Error::MissingSocketPath)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Call any Herdr socket method and deserialize its `result` object.
    pub fn call<P, R>(&self, method: &str, params: &P) -> Result<R, Error>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        serde_json::from_value(self.call_value(method, params)?).map_err(Error::Json)
    }

    /// Call any Herdr socket method without requiring a result type.
    pub fn call_value<P>(&self, method: &str, params: &P) -> Result<Value, Error>
    where
        P: Serialize + ?Sized,
    {
        let id = self.request_id();
        let mut stream = self.connect()?;
        self.set_timeouts(&stream)?;
        write_request(&mut stream, &id, method, params)?;
        read_response(&mut BufReader::new(stream), &id)
    }

    pub fn ping(&self) -> Result<PingResult, Error> {
        self.call("ping", &json!({}))
    }

    pub fn snapshot(&self) -> Result<SessionSnapshot, Error> {
        let result: SnapshotResult = self.call("session.snapshot", &json!({}))?;
        Ok(result.snapshot)
    }

    pub fn workspaces(&self) -> Result<Vec<WorkspaceInfo>, Error> {
        let result: WorkspaceListResult = self.call("workspace.list", &json!({}))?;
        Ok(result.workspaces)
    }

    pub fn tabs(&self, workspace_id: Option<&str>) -> Result<Vec<TabInfo>, Error> {
        let result: TabListResult = self.call(
            "tab.list",
            &workspace_id
                .map(|id| json!({ "workspace_id": id }))
                .unwrap_or_else(|| json!({})),
        )?;
        Ok(result.tabs)
    }

    pub fn panes(&self, workspace_id: Option<&str>) -> Result<Vec<PaneInfo>, Error> {
        let result: PaneListResult = self.call(
            "pane.list",
            &workspace_id
                .map(|id| json!({ "workspace_id": id }))
                .unwrap_or_else(|| json!({})),
        )?;
        Ok(result.panes)
    }

    pub fn current_pane(&self, caller_pane_id: Option<&str>) -> Result<PaneInfo, Error> {
        let result: PaneResult = self.call(
            "pane.current",
            &caller_pane_id
                .map(|id| json!({ "caller_pane_id": id }))
                .unwrap_or_else(|| json!({})),
        )?;
        Ok(result.pane)
    }

    pub fn pane(&self, pane_id: &str) -> Result<PaneInfo, Error> {
        let result: PaneResult = self.call("pane.get", &json!({ "pane_id": pane_id }))?;
        Ok(result.pane)
    }

    pub fn agents(&self) -> Result<Vec<AgentInfo>, Error> {
        let result: AgentListResult = self.call("agent.list", &json!({}))?;
        Ok(result.agents)
    }

    /// Start a long-lived event subscription on a dedicated connection.
    pub fn subscribe(&self, subscriptions: &[EventSubscription]) -> Result<Subscription, Error> {
        let id = self.request_id();
        let mut stream = self.connect()?;
        self.set_timeouts(&stream)?;
        write_request(
            &mut stream,
            &id,
            "events.subscribe",
            &json!({ "subscriptions": subscriptions }),
        )?;

        let mut reader = BufReader::new(stream);
        let ack: SubscriptionStarted =
            serde_json::from_value(read_response(&mut reader, &id)?).map_err(Error::Json)?;
        if ack.kind != "subscription_started" {
            return Err(Error::UnexpectedResult(ack.kind));
        }
        clear_recv_timeout_best_effort(reader.get_ref())?;
        Ok(Subscription { reader })
    }

    fn request_id(&self) -> String {
        format!(
            "herdr-client:{}:{}",
            std::process::id(),
            self.next_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn connect(&self) -> Result<LocalStream, Error> {
        connect_local_stream(&self.socket_path).map_err(Error::Io)
    }

    fn set_timeouts(&self, stream: &LocalStream) -> Result<(), Error> {
        let Some(timeout) = self.timeout else {
            return Ok(());
        };
        set_timeout_best_effort(stream, TimeoutKind::Send, Some(timeout))?;
        set_timeout_best_effort(stream, TimeoutKind::Recv, Some(timeout))
    }
}

pub struct Subscription {
    reader: BufReader<LocalStream>,
}

impl Subscription {
    pub fn next_event(&mut self) -> Result<Option<EventEnvelope>, Error> {
        let Some(value) = read_json_line::<Value, _>(&mut self.reader)? else {
            return Ok(None);
        };
        if let Some(error) = value.get("error") {
            return Err(Error::Api(
                serde_json::from_value(error.clone()).map_err(Error::Json)?,
            ));
        }
        serde_json::from_value(value).map(Some).map_err(Error::Json)
    }
}

impl Iterator for Subscription {
    type Item = Result<EventEnvelope, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_event().transpose()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug)]
pub enum Error {
    MissingSocketPath,
    Io(io::Error),
    Json(serde_json::Error),
    Api(ApiError),
    EmptyResponse,
    MissingResult,
    MismatchedResponseId { expected: String, actual: String },
    UnexpectedResult(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSocketPath => f.write_str("HERDR_SOCKET_PATH is not set"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
            Self::Api(error) => write!(f, "{error}"),
            Self::EmptyResponse => f.write_str("Herdr closed the socket without a response"),
            Self::MissingResult => f.write_str("Herdr response has neither result nor error"),
            Self::MismatchedResponseId { expected, actual } => {
                write!(
                    f,
                    "Herdr response id {actual:?} does not match {expected:?}"
                )
            }
            Self::UnexpectedResult(kind) => write!(f, "unexpected Herdr result type {kind:?}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Api(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Serialize)]
struct Request<'a, P: ?Sized> {
    id: &'a str,
    method: &'a str,
    params: &'a P,
}

#[derive(Deserialize)]
struct Response {
    id: String,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct SnapshotResult {
    #[serde(rename = "type")]
    _kind: String,
    snapshot: SessionSnapshot,
}

#[derive(Deserialize)]
struct WorkspaceListResult {
    #[serde(rename = "type")]
    _kind: String,
    workspaces: Vec<WorkspaceInfo>,
}

#[derive(Deserialize)]
struct TabListResult {
    #[serde(rename = "type")]
    _kind: String,
    tabs: Vec<TabInfo>,
}

#[derive(Deserialize)]
struct PaneListResult {
    #[serde(rename = "type")]
    _kind: String,
    panes: Vec<PaneInfo>,
}

#[derive(Deserialize)]
struct PaneResult {
    #[serde(rename = "type")]
    _kind: String,
    pane: PaneInfo,
}

#[derive(Deserialize)]
struct AgentListResult {
    #[serde(rename = "type")]
    _kind: String,
    agents: Vec<AgentInfo>,
}

#[derive(Deserialize)]
struct SubscriptionStarted {
    #[serde(rename = "type")]
    kind: String,
}

fn write_request<P: Serialize + ?Sized>(
    stream: &mut LocalStream,
    id: &str,
    method: &str,
    params: &P,
) -> Result<(), Error> {
    serde_json::to_writer(&mut *stream, &Request { id, method, params }).map_err(Error::Json)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn read_response<R: BufRead>(reader: &mut R, expected_id: &str) -> Result<Value, Error> {
    let response = read_json_line::<Response, _>(reader)?.ok_or(Error::EmptyResponse)?;
    if response.id != expected_id {
        return Err(Error::MismatchedResponseId {
            expected: expected_id.to_owned(),
            actual: response.id,
        });
    }
    if let Some(error) = response.error {
        return Err(Error::Api(error));
    }
    response.result.ok_or(Error::MissingResult)
}

fn read_json_line<T: DeserializeOwned, R: BufRead>(reader: &mut R) -> Result<Option<T>, Error> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line).map(Some).map_err(Error::Json)
}

fn connect_local_stream(path: &Path) -> io::Result<LocalStream> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::{prelude::*, GenericFilePath};

        LocalStream::connect(path.to_fs_name::<GenericFilePath>()?)
    }

    #[cfg(windows)]
    {
        use interprocess::local_socket::{prelude::*, GenericNamespaced};

        let name = path.to_string_lossy().to_string();
        LocalStream::connect(name.to_ns_name::<GenericNamespaced>()?)
    }
}

enum TimeoutKind {
    Send,
    Recv,
}

fn set_timeout_best_effort(
    stream: &LocalStream,
    kind: TimeoutKind,
    timeout: Option<Duration>,
) -> Result<(), Error> {
    let result = match kind {
        TimeoutKind::Send => stream.set_send_timeout(timeout),
        TimeoutKind::Recv => stream.set_recv_timeout(timeout),
    };
    match result {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(()),
        Err(error) => Err(Error::Io(error)),
    }
}

fn clear_recv_timeout_best_effort(stream: &LocalStream) -> Result<(), Error> {
    set_timeout_best_effort(stream, TimeoutKind::Recv, None)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::thread;

    use super::*;

    fn socket_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("herdr-client-{name}-{}.sock", std::process::id()))
    }

    #[test]
    fn ping_round_trips_over_ndjson() {
        let path = socket_path("ping");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind test socket");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone stream"))
                .read_line(&mut request)
                .expect("read request");
            let request: Value = serde_json::from_str(&request).expect("parse request");
            assert_eq!(request["method"], "ping");
            writeln!(
                stream,
                "{}",
                json!({
                    "id": request["id"],
                    "result": {
                        "type": "pong",
                        "version": "0.7.5",
                        "protocol": 17
                    }
                })
            )
            .expect("write response");
        });

        let ping = Client::new(&path).ping().expect("ping");
        assert_eq!(ping.version, "0.7.5");
        assert_eq!(ping.protocol, 17);
        server.join().expect("server thread");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn api_errors_keep_the_server_code() {
        let line = br#"{"id":"x","error":{"code":"not_found","message":"pane not found"}}
"#;
        let error = read_response(&mut &line[..], "x").expect_err("error response");
        match error {
            Error::Api(error) => assert_eq!(error.code, "not_found"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn subscriptions_keep_reading_after_the_ack() {
        let path = socket_path("subscribe");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind test socket");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone stream"))
                .read_line(&mut request)
                .expect("read request");
            let request: Value = serde_json::from_str(&request).expect("parse request");
            assert_eq!(request["method"], "events.subscribe");
            writeln!(
                stream,
                "{}",
                json!({
                    "id": request["id"],
                    "result": { "type": "subscription_started" }
                })
            )
            .expect("write ack");
            writeln!(
                stream,
                "{}",
                json!({
                    "event": "pane.focused",
                    "data": { "type": "pane_focused", "pane_id": "w1:p1" }
                })
            )
            .expect("write event");
        });

        let mut subscription = Client::new(&path)
            .subscribe(&[EventSubscription::new("pane.focused")])
            .expect("subscribe");
        let event = subscription
            .next_event()
            .expect("read event")
            .expect("event before close");
        assert_eq!(event.event, "pane.focused");
        assert_eq!(event.data["pane_id"], "w1:p1");
        server.join().expect("server thread");
        let _ = std::fs::remove_file(path);
    }
}
