//! An in-process stand-in for a Herdr daemon's socket API, limited to the
//! client methods. Tests never touch a real Herdr socket.

use std::{
    io::{BufRead, BufReader, Write},
    net::Shutdown,
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use herdr_client::{ClientInfo, sessions::LocalSession};
use serde_json::{Value, json};

#[derive(Default)]
struct State {
    boot_id: String,
    clients: Vec<ClientInfo>,
    subscribers: Vec<UnixStream>,
    /// The error `client.list` is answered with, while there is one.
    list_error: Option<Value>,
    /// The `client.jump` answer: `{"result": ..}` or `{"error": ..}`.
    jump_reply: Value,
    /// `client.jump` requests left unanswered, oldest first, while held.
    held_jumps: Option<Vec<(UnixStream, Value)>>,
    requests: Vec<Value>,
}

pub struct MockDaemon {
    pub name: String,
    pub socket_path: PathBuf,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    _directory: tempfile::TempDir,
}

impl MockDaemon {
    pub fn start(name: &str) -> Self {
        let directory = tempfile::Builder::new()
            .prefix("hc-")
            .tempdir()
            .expect("temporary socket directory");
        let socket_path = directory.path().join("herdr.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind mock daemon");
        listener.set_nonblocking(true).expect("nonblocking accept");
        let state = Arc::new(Mutex::new(State {
            boot_id: format!("boot-{name}"),
            jump_reply: Value::Null,
            ..State::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::spawn({
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => serve(stream, &state),
                        Err(_) => thread::sleep(Duration::from_millis(2)),
                    }
                }
            }
        });
        Self {
            name: name.into(),
            socket_path,
            state,
            stop,
            thread: Some(thread),
            _directory: directory,
        }
    }

    pub fn session(&self) -> LocalSession {
        LocalSession {
            name: self.name.clone(),
            socket_path: self.socket_path.clone(),
        }
    }

    /// Attaches the client or replaces its report, and pushes `client.changed`.
    pub fn set_client(&self, client: ClientInfo) {
        self.set_clients(vec![client]);
    }

    /// [`Self::set_client`] for each client, with every `client.changed`
    /// event written to a subscriber at once, as one burst.
    pub fn set_clients(&self, clients: Vec<ClientInfo>) {
        let mut state = self.state.lock().unwrap();
        let mut burst = String::new();
        for client in clients {
            state
                .clients
                .retain(|other| other.client_id != client.client_id);
            burst.push_str(&format!("{}\n", changed(client.client_id)));
            state.clients.push(client);
        }
        state.clients.sort_by_key(|client| client.client_id);
        push(&mut state, burst.as_bytes());
    }

    /// Detaches the client and pushes `client.changed`.
    pub fn remove_client(&self, client_id: u64) {
        let mut state = self.state.lock().unwrap();
        state.clients.retain(|client| client.client_id != client_id);
        push(&mut state, format!("{}\n", changed(client_id)).as_bytes());
    }

    /// Pushes a raw line to every subscriber.
    pub fn push_event(&self, event: &Value) {
        push(
            &mut self.state.lock().unwrap(),
            format!("{event}\n").as_bytes(),
        );
    }

    /// Answers every `client.list` with `error` (`{"code": .., "message":
    /// ..}`) until it is `None` again.
    pub fn fail_listing(&self, error: Option<Value>) {
        self.state.lock().unwrap().list_error = error;
    }

    pub fn set_jump_reply(&self, reply: Value) {
        self.state.lock().unwrap().jump_reply = reply;
    }

    /// Leaves every `client.jump` from now on unanswered, as a daemon does
    /// while a client performs the jump, until [`Self::answer_jumps`].
    pub fn hold_jumps(&self) {
        self.state.lock().unwrap().held_jumps = Some(Vec::new());
    }

    /// Answers the held jumps, oldest first, with the reply set now, and
    /// stops holding.
    pub fn answer_jumps(&self) {
        let mut state = self.state.lock().unwrap();
        for (mut stream, id) in state.held_jumps.take().unwrap_or_default() {
            let mut reply = state.jump_reply.clone();
            reply["id"] = id;
            let _ = writeln!(stream, "{reply}");
        }
    }

    /// Every request received so far for `method`.
    pub fn requests(&self, method: &str) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request["method"] == method)
            .cloned()
            .collect()
    }
}

impl Drop for MockDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        for stream in self.state.lock().unwrap().subscribers.drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

/// The daemon's `client.changed` event, which names the client only.
fn changed(client_id: u64) -> Value {
    json!({"event": "client_changed", "data": {
        "type": "client_changed", "client_id": client_id,
    }})
}

/// Writes `lines` to every subscriber in one write.
fn push(state: &mut State, lines: &[u8]) {
    state
        .subscribers
        .retain_mut(|stream| stream.write_all(lines).is_ok());
}

fn serve(mut stream: UnixStream, state: &Mutex<State>) {
    stream.set_nonblocking(false).expect("blocking stream");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let request: Value = serde_json::from_str(&line).expect("request is JSON");
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    let mut state = state.lock().unwrap();
    state.requests.push(request.clone());
    let reply = match method.as_str() {
        "events.subscribe" => json!({"id": id, "result": {"type": "subscription_started"}}),
        "client.list" => match &state.list_error {
            Some(error) => json!({"id": id, "error": error}),
            None => json!({"id": id, "result": {
                "type": "client_list", "boot_id": state.boot_id, "clients": state.clients,
            }}),
        },
        "client.jump" => {
            if let Some(held) = &mut state.held_jumps {
                held.push((stream, id));
                return;
            }
            let mut reply = state.jump_reply.clone();
            reply["id"] = id;
            reply
        }
        other => json!({"id": id, "error": {
            "code": "invalid_request",
            "message": format!("invalid request: unknown variant `{other}`"),
        }}),
    };
    let _ = writeln!(stream, "{reply}");
    if method == "events.subscribe" && reply.get("result").is_some() {
        state.subscribers.push(stream);
    }
}
