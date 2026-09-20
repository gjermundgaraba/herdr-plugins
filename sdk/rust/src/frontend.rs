//! Direct protocol-7 connection to one local Herdr TUI. Mutations are never retried.
use crate::AgentStatus;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{self, BufReader, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const PROTOCOL: u64 = 7;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// An endpoint and the server boot whose ids the caller captured. Pane, tab,
/// and workspace ids are per-server counters, so the TUI rejects a route whose
/// boot no longer matches instead of acting on a reused id.
pub struct Route {
    pub endpoint_id: String,
    /// Absent while the endpoint has no server identity yet; an available
    /// endpoint always carries one.
    pub boot_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputTarget {
    pub endpoint_id: String,
    pub pane_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub client_id: String,
    pub revision: u64,
    pub focused: Option<bool>,
    pub input_ready: bool,
    pub active_endpoint_id: Option<String>,
    pub input_target: Option<InputTarget>,
    pub endpoints: Vec<Endpoint>,
}
impl Snapshot {
    pub fn route(&self, endpoint_id: &str) -> Option<Route> {
        let e = self
            .endpoints
            .iter()
            .find(|e| e.endpoint_id == endpoint_id && e.is_available())?;
        Some(Route {
            endpoint_id: e.endpoint_id.clone(),
            boot_id: e.boot_id.clone(),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub endpoint_id: String,
    pub label: String,
    pub status: String,
    pub boot_id: Option<String>,
    pub snapshot: Option<ShellSnapshot>,
}
impl Endpoint {
    pub fn is_available(&self) -> bool {
        self.status == "online"
            && self.boot_id.as_ref().is_some_and(|v| !v.is_empty())
            && self.snapshot.is_some()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellSnapshot {
    pub boot_id: String,
    pub revision: u64,
    pub focused_workspace_id: Option<String>,
    pub focused_tab_id: Option<String>,
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<Workspace>,
    pub tabs: Vec<Tab>,
    pub panes: Vec<Pane>,
    pub agents: Vec<Agent>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    pub active_tab_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub agent_status: AgentStatus,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub agent_status: AgentStatus,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub label: Option<String>,
    pub cwd: Option<String>,
    pub foreground_cwd: Option<String>,
    pub focused: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub name: Option<String>,
    pub display_agent: Option<String>,
    pub agent: Option<String>,
    pub title: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_status: AgentStatus,
    pub state_change_seq: u64,
    pub state_labels: Vec<(String, String)>,
    pub tokens: Vec<(String, String)>,
    pub focused: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavigationTarget {
    Workspace(String),
    Tab(String),
    Pane(String),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Input {
    Text(String),
    Keys(Vec<String>),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAccepted {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct FrontendError {
    pub code: String,
    pub message: String,
}
impl FrontendError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl From<io::Error> for FrontendError {
    fn from(e: io::Error) -> Self {
        Self::new(
            if matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) {
                "timeout"
            } else {
                "io"
            },
            e.to_string(),
        )
    }
}
impl From<serde_json::Error> for FrontendError {
    fn from(e: serde_json::Error) -> Self {
        Self::new("json", e.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct FrontendClient {
    path: PathBuf,
    timeout: Duration,
}
impl FrontendClient {
    pub fn from_env() -> Result<Self, FrontendError> {
        env::var_os("HERDR_FRONTEND_SOCKET")
            .filter(|v| !v.is_empty())
            .map(Self::connect)
            .ok_or_else(|| FrontendError::new("environment", "HERDR_FRONTEND_SOCKET is not set"))
    }
    /// Construct a handle; each operation handshakes a fresh connection.
    pub fn connect(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            timeout: Duration::from_secs(5),
        }
    }
    pub fn socket_path(&self) -> &Path {
        &self.path
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    fn open(&self) -> Result<(BufReader<UnixStream>, Instant), FrontendError> {
        let deadline = Instant::now() + self.timeout;
        let stream = UnixStream::connect(&self.path)?;
        stream.set_write_timeout(Some(self.timeout))?;
        let mut reader = BufReader::new(stream);
        let hello = read(
            &mut reader,
            &mut Vec::new(),
            deadline.saturating_duration_since(Instant::now()),
        )?
        .ok_or_else(eof)?;
        if hello["type"] == "error" {
            return Err(serde_json::from_value(hello["error"].clone())?);
        }
        if hello["type"] != "hello"
            || hello["protocol"] != PROTOCOL
            || hello["client_id"].as_str().is_none_or(str::is_empty)
        {
            return Err(FrontendError::new(
                "protocol",
                "expected frontend protocol 7 hello",
            ));
        }
        Ok((reader, deadline))
    }
    fn request(&self, mut request: Value) -> Result<Value, FrontendError> {
        let (mut reader, deadline) = self.open()?;
        request["protocol"] = json!(PROTOCOL);
        request["id"] = json!(1);
        write(reader.get_mut(), &request)?;
        let reply = read(
            &mut reader,
            &mut Vec::new(),
            deadline.saturating_duration_since(Instant::now()),
        )?
        .ok_or_else(eof)?;
        validate(&reply)?;
        Ok(reply)
    }
    pub fn snapshot(&self) -> Result<Snapshot, FrontendError> {
        let reply = self.request(json!({"type":"snapshot"}))?;
        snapshot_reply(reply)
    }
    /// Select a route's target. On the active endpoint the reply is the focus
    /// call's result; elsewhere it settles on the observed activation or the
    /// deadline. Any `Ok` means the target is focused.
    pub fn navigate(
        &self,
        route: &Route,
        target: &NavigationTarget,
    ) -> Result<Value, FrontendError> {
        self.result(json!({"type":"select","route":route,"target":target}))
    }
    /// Ordinary input follows the TUI's own focus, including overlays.
    pub fn input(&self, input: &Input) -> Result<InputAccepted, FrontendError> {
        let mut request = serde_json::to_value(input)?;
        request["type"] = json!("input");
        self.result(request)
    }
    /// Run one advertised endpoint method through the route's endpoint, which
    /// must be the active endpoint with an available lease.
    pub fn call(&self, route: &Route, method: &str, params: Value) -> Result<Value, FrontendError> {
        self.result(json!({"type":"call","route":route,"method":method,"params":params}))
    }
    fn result<R: DeserializeOwned>(&self, request: Value) -> Result<R, FrontendError> {
        let reply = self.request(request)?;
        if reply["type"] != "reply" {
            return Err(FrontendError::new("protocol", "expected reply"));
        }
        Ok(serde_json::from_value(
            reply
                .get("result")
                .cloned()
                .ok_or_else(|| FrontendError::new("protocol", "missing result"))?,
        )?)
    }
    pub fn subscribe(&self) -> Result<Subscription, FrontendError> {
        let (mut reader, deadline) = self.open()?;
        write(
            reader.get_mut(),
            &json!({"type":"subscribe","protocol":PROTOCOL,"id":1}),
        )?;
        let first = read(
            &mut reader,
            &mut Vec::new(),
            deadline.saturating_duration_since(Instant::now()),
        )?
        .ok_or_else(eof)?;
        validate(&first)?;
        Ok(Subscription {
            reader,
            pending: Vec::new(),
            first: Some(snapshot_reply(first)?),
            ended: false,
        })
    }
}
pub struct Subscription {
    reader: BufReader<UnixStream>,
    pending: Vec<u8>,
    first: Option<Snapshot>,
    ended: bool,
}
impl Subscription {
    /// No heartbeat or freshness timeout: a quiet connected TUI is still live.
    pub fn next_snapshot(&mut self, timeout: Duration) -> Result<Option<Snapshot>, FrontendError> {
        if let Some(first) = self.first.take() {
            return Ok(Some(first));
        }
        if self.ended {
            return Ok(None);
        }
        let Some(reply) = read(&mut self.reader, &mut self.pending, timeout)? else {
            self.ended = true;
            return Ok(None);
        };
        validate(&reply)?;
        snapshot_reply(reply).map(Some)
    }
}
fn eof() -> FrontendError {
    FrontendError::new(
        "eof",
        "frontend disconnected; mutation outcome may be unknown",
    )
}
fn read(
    reader: &mut BufReader<UnixStream>,
    pending: &mut Vec<u8>,
    timeout: Duration,
) -> Result<Option<Value>, FrontendError> {
    crate::ndjson::read_frame_with_timeout(reader, pending, timeout)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
        .transpose()
}
fn write(stream: &mut UnixStream, value: &Value) -> Result<(), FrontendError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    if bytes.len() > crate::ndjson::MAX_FRAME_BYTES {
        return Err(FrontendError::new(
            "invalid_request",
            "request exceeds 1 MiB",
        ));
    }
    stream.write_all(&bytes)?;
    Ok(())
}
fn validate(reply: &Value) -> Result<(), FrontendError> {
    if reply["id"] != 1 {
        return Err(FrontendError::new("protocol", "response ID mismatch"));
    }
    if reply["type"] == "error" {
        return Err(serde_json::from_value(reply["error"].clone())?);
    }
    Ok(())
}
fn snapshot_reply(reply: Value) -> Result<Snapshot, FrontendError> {
    if reply["type"] != "snapshot" {
        return Err(FrontendError::new("protocol", "expected snapshot"));
    }
    Ok(serde_json::from_value(reply["snapshot"].clone())?)
}
pub fn directory() -> PathBuf {
    env::var_os("HERDR_CLIENT_API_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: geteuid has no preconditions.
            PathBuf::from(format!("/tmp/herdr-clients-{}", unsafe { libc::geteuid() }))
        })
}
pub fn discover(dir: &Path) -> Vec<PathBuf> {
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return Vec::new();
    };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let m = fs::symlink_metadata(&path).ok()?;
            (path.extension().is_some_and(|s| s == "sock")
                && m.file_type().is_socket()
                && m.uid() == uid
                && m.mode() & 0o077 == 0)
                .then_some(path)
        })
        .collect();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::net::UnixListener,
        sync::atomic::{AtomicU64, Ordering},
        thread,
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn serve(
        f: impl FnOnce(BufReader<UnixStream>) + Send + 'static,
    ) -> (FrontendClient, thread::JoinHandle<()>, PathBuf) {
        let path = PathBuf::from(format!(
            "/tmp/hf-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let join = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            write(
                &mut socket,
                &json!({"type":"hello","protocol":PROTOCOL,"client_id":"c"}),
            )
            .unwrap();
            f(BufReader::new(socket));
        });
        (
            FrontendClient::connect(&path).with_timeout(Duration::from_secs(1)),
            join,
            path,
        )
    }
    fn snapshot() -> Value {
        json!({"client_id":"c","revision":1,"focused":true,"input_ready":false,"active_endpoint_id":null,"input_target":null,"endpoints":[]})
    }
    fn route() -> Route {
        Route {
            endpoint_id: "local".into(),
            boot_id: Some("boot".into()),
        }
    }
    #[test]
    fn select_input_and_call_preserve_wire_contract() {
        for kind in ["select", "input", "call"] {
            let (client, join, path) = serve(move |mut reader| {
                let r = read(&mut reader, &mut vec![], Duration::from_secs(1))
                    .unwrap()
                    .unwrap();
                assert_eq!(r["protocol"], PROTOCOL);
                assert_eq!(r["id"], 1);
                assert_eq!(r["type"], kind);
                match kind {
                    "input" => {
                        assert_eq!(r["keys"], json!(["ctrl+c", "enter"]));
                        assert!(r.get("route").is_none());
                    }
                    "select" => {
                        assert_eq!(r["route"], json!(route()));
                        assert_eq!(r["target"], json!({"pane":"p"}));
                    }
                    _ => {
                        assert_eq!(r["route"], json!(route()));
                        assert_eq!(r["method"], "agent.prompt");
                        assert_eq!(r["params"], json!({"target":"p","text":"hello"}));
                    }
                }
                let result = json!({"ok":true});
                write(
                    reader.get_mut(),
                    &json!({"type":"reply","id":1,"result":result}),
                )
                .unwrap();
            });
            match kind {
                "input" => assert!(
                    client
                        .input(&Input::Keys(vec!["ctrl+c".into(), "enter".into()]))
                        .unwrap()
                        .ok
                ),
                "select" => {
                    let reply = client
                        .navigate(&route(), &NavigationTarget::Pane("p".into()))
                        .unwrap();
                    assert_eq!(reply["ok"], true);
                }
                _ => {
                    let reply = client
                        .call(
                            &route(),
                            "agent.prompt",
                            json!({"target":"p","text":"hello"}),
                        )
                        .unwrap();
                    assert_eq!(reply["ok"], true);
                }
            }
            join.join().unwrap();
            fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn errors_eof_and_malformed_replies_surface_as_errors() {
        for (response, code) in [
            (
                json!({"type":"error","id":1,"error":{"code":"cancelled","message":"retired"}}),
                "cancelled",
            ),
            (json!({"type":"reply","id":9,"result":{}}), "protocol"),
            (Value::Null, "eof"),
        ] {
            let (client, join, path) = serve(move |mut reader| {
                let r = read(&mut reader, &mut vec![], Duration::from_secs(1))
                    .unwrap()
                    .unwrap();
                assert_eq!(r["type"], "call");
                if !response.is_null() {
                    write(reader.get_mut(), &response).unwrap();
                }
            });
            let error = client
                .call(
                    &route(),
                    "pane.focus_direction",
                    json!({"pane_id":"p","direction":"left"}),
                )
                .unwrap_err();
            assert_eq!(error.code, code);
            join.join().unwrap();
            fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn subscription_push_is_not_polled_and_quiet_timeout_does_not_end_it() {
        let (client, join, path) = serve(|mut reader| {
            let r = read(&mut reader, &mut vec![], Duration::from_secs(1))
                .unwrap()
                .unwrap();
            assert_eq!(r["type"], "subscribe");
            write(
                reader.get_mut(),
                &json!({"type":"snapshot","id":1,"snapshot":snapshot()}),
            )
            .unwrap();
            thread::sleep(Duration::from_millis(75));
            let mut s = snapshot();
            s["revision"] = json!(2);
            write(
                reader.get_mut(),
                &json!({"type":"snapshot","id":1,"snapshot":s}),
            )
            .unwrap();
        });
        let mut subscription = client.subscribe().unwrap();
        assert_eq!(
            subscription
                .next_snapshot(Duration::ZERO)
                .unwrap()
                .unwrap()
                .revision,
            1
        );
        assert_eq!(
            subscription
                .next_snapshot(Duration::from_millis(5))
                .unwrap_err()
                .code,
            "timeout"
        );
        assert_eq!(
            subscription
                .next_snapshot(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .revision,
            2
        );
        assert!(
            subscription
                .next_snapshot(Duration::from_secs(1))
                .unwrap()
                .is_none()
        );
        join.join().unwrap();
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn admission_failure_and_old_hello_are_reported_before_any_request() {
        for (hello, expected) in [
            (
                json!({"type":"hello","protocol":6,"client_id":"c"}),
                "protocol",
            ),
            (
                json!({"type":"error","id":null,"error":{"code":"request_too_large","message":"request exceeds one MiB"}}),
                "request_too_large",
            ),
        ] {
            let path = PathBuf::from(format!(
                "/tmp/hf-old-{}-{}.sock",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let listener = UnixListener::bind(&path).unwrap();
            let join = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                write(&mut stream, &hello).unwrap();
                use std::io::Read;
                let mut byte = [0];
                assert_eq!(stream.read(&mut byte).unwrap(), 0);
            });
            assert_eq!(
                FrontendClient::connect(&path).snapshot().unwrap_err().code,
                expected
            );
            join.join().unwrap();
            fs::remove_file(path).unwrap();
        }
    }
}
