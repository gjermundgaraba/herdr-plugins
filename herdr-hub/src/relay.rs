use std::{
    collections::HashSet,
    io::{self, BufReader, BufWriter, Write},
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use herdr_client::ndjson;
use herdr_hub_client::{
    ClientMessage, Error as HubError, HubClient, Model, PROTOCOL, ServerMessage, Stream,
};
use serde::Serialize;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const CALL_TIMEOUT: Duration = Duration::from_secs(3);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(250);
const STARTUP_POLL: Duration = Duration::from_millis(25);
const QUEUE_CAPACITY: usize = 64;
const MAX_IN_FLIGHT: usize = 64;

pub fn run(resolve_herdr: impl FnOnce() -> Result<PathBuf>) -> Result<()> {
    let client = HubClient::new();
    let (stream, model) = subscribe_or_start(&client, resolve_herdr)?;
    let (events, incoming) = mpsc::sync_channel(QUEUE_CAPACITY);
    let (outgoing, output) = mpsc::sync_channel(QUEUE_CAPACITY);

    let writer = spawn_output(output, events.clone())?;
    let (filter, hello) = LocalFilter::from_model(model);
    send(&outgoing, hello)?;
    spawn_subscription(stream, filter, events.clone())?;
    spawn_input(events.clone())?;

    let result = event_loop(&client, &events, &incoming, &outgoing);
    drop(outgoing);
    let writer_result = writer
        .join()
        .map_err(|_| anyhow!("relay output writer panicked"))?;
    result.and(writer_result)
}

fn subscribe_or_start(
    client: &HubClient,
    resolve_herdr: impl FnOnce() -> Result<PathBuf>,
) -> Result<(Stream, Model)> {
    match client.subscribe(CONNECT_TIMEOUT) {
        Ok(subscription) => return Ok(subscription),
        Err(error) if hub_is_absent(&error) => {}
        Err(error) => return Err(error).context("cannot subscribe to the local Herdr Hub"),
    }

    let herdr = resolve_herdr().context("cannot find herdr to start the local Herdr Hub")?;
    let (finished, result) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("herdr-hub-relay-local".into())
        .spawn(move || {
            let outcome = crate::hub::run(&herdr).map_err(|error| format!("{error:#}"));
            let _ = finished.send(outcome);
        })
        .context("cannot start in-process Herdr Hub")?;

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut last_error: String;
    let mut hub_result = None;
    loop {
        match client.subscribe(STARTUP_ATTEMPT_TIMEOUT) {
            Ok(subscription) => return Ok(subscription),
            Err(error) if startup_is_pending(&error) => last_error = error.to_string(),
            Err(error) => {
                return Err(error).context("cannot subscribe to the in-process Herdr Hub");
            }
        }
        if hub_result.is_none() {
            hub_result = result.try_recv().ok();
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(STARTUP_POLL);
    }

    match hub_result {
        Some(Err(error)) => bail!("in-process Herdr Hub failed to start: {error}"),
        Some(Ok(())) => bail!("in-process Herdr Hub stopped before the relay connected"),
        None => bail!("in-process Herdr Hub did not become ready: {last_error}"),
    }
}

fn hub_is_absent(error: &HubError) -> bool {
    matches!(
        error,
        HubError::Io(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            )
    )
}

fn startup_is_pending(error: &HubError) -> bool {
    hub_is_absent(error)
        || matches!(
            error,
            HubError::Io(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                )
        )
        || matches!(error, HubError::Disconnected)
}

fn event_loop(
    client: &HubClient,
    events: &SyncSender<Event>,
    incoming: &Receiver<Event>,
    outgoing: &SyncSender<ServerMessage>,
) -> Result<()> {
    let mut calls = 0_usize;
    let mut input_closed = false;
    loop {
        match incoming.recv().context("relay workers stopped")? {
            Event::Input(message) => {
                match start_call(client, message, events.clone(), calls >= MAX_IN_FLIGHT) {
                    Ok(Some(reply)) => send(outgoing, reply)?,
                    Ok(None) => calls += 1,
                    Err(error) => {
                        let message = error.to_string();
                        let _ = send(
                            outgoing,
                            ServerMessage::Error {
                                error: message.clone(),
                            },
                        );
                        bail!(message)
                    }
                }
            }
            Event::InputClosed => {
                input_closed = true;
                if calls == 0 {
                    return Ok(());
                }
            }
            Event::InputFailed(error) => {
                let _ = send(
                    outgoing,
                    ServerMessage::Error {
                        error: error.clone(),
                    },
                );
                bail!("cannot read relay input: {error}")
            }
            Event::CallFinished(message) => {
                calls = calls
                    .checked_sub(1)
                    .ok_or_else(|| anyhow!("relay received an unexpected call reply"))?;
                send(outgoing, message)?;
                if input_closed && calls == 0 {
                    return Ok(());
                }
            }
            Event::Local(message) => send(outgoing, message)?,
            Event::LocalClosed => bail!("local Herdr Hub disconnected"),
            Event::LocalFailed(error) => bail!("local Herdr Hub stream failed: {error}"),
            Event::OutputFailed(error) => bail!("cannot write relay output: {error}"),
        }
    }
}

fn start_call(
    client: &HubClient,
    message: ClientMessage,
    events: SyncSender<Event>,
    saturated: bool,
) -> Result<Option<ServerMessage>> {
    let ClientMessage::Call {
        protocol,
        id,
        session,
        method,
        params,
    } = message
    else {
        bail!("relay input accepts only call messages")
    };
    if protocol != PROTOCOL {
        return Ok(Some(ServerMessage::ReplyError {
            id,
            error: format!("hub protocol {protocol} does not match relay protocol {PROTOCOL}"),
        }));
    }
    if !session.starts_with("local/") {
        return Ok(Some(ServerMessage::ReplyError {
            id,
            error: "relay calls must target a local session".into(),
        }));
    }
    if saturated {
        return Ok(Some(ServerMessage::ReplyError {
            id,
            error: "relay has too many calls in flight".into(),
        }));
    }

    let client = client.clone();
    let spawned = thread::Builder::new()
        .name("herdr-hub-relay-call".into())
        .spawn(move || {
            let message = match client.call(&session, &method, params, CALL_TIMEOUT) {
                Ok(result) => ServerMessage::Reply { id, result },
                Err(error) => ServerMessage::ReplyError {
                    id,
                    error: error.to_string(),
                },
            };
            let _ = events.send(Event::CallFinished(message));
        });
    match spawned {
        Ok(_) => Ok(None),
        Err(error) => Ok(Some(ServerMessage::ReplyError {
            id,
            error: format!("cannot start relay call: {error}"),
        })),
    }
}

fn spawn_input(events: SyncSender<Event>) -> Result<()> {
    thread::Builder::new()
        .name("herdr-hub-relay-input".into())
        .spawn(move || {
            let stdin = io::stdin();
            let mut reader = BufReader::new(stdin.lock());
            let mut pending = Vec::new();
            loop {
                let event = match ndjson::read_frame(&mut reader, &mut pending) {
                    Ok(Some(frame)) => match serde_json::from_slice(&frame) {
                        Ok(message) => Event::Input(message),
                        Err(error) => Event::InputFailed(error.to_string()),
                    },
                    Ok(None) => Event::InputClosed,
                    Err(error) => Event::InputFailed(error.to_string()),
                };
                let stop = !matches!(event, Event::Input(_));
                if events.send(event).is_err() || stop {
                    return;
                }
            }
        })
        .context("cannot start relay input reader")?;
    Ok(())
}

fn spawn_subscription(
    mut stream: Stream,
    mut filter: LocalFilter,
    events: SyncSender<Event>,
) -> Result<()> {
    thread::Builder::new()
        .name("herdr-hub-relay-subscription".into())
        .spawn(move || {
            loop {
                let event = match stream.next() {
                    Ok(Some(message)) => match filter.message(message) {
                        Some(message) => Event::Local(message),
                        None => continue,
                    },
                    Ok(None) => Event::LocalClosed,
                    Err(error) => Event::LocalFailed(error.to_string()),
                };
                let stop = !matches!(event, Event::Local(_));
                if events.send(event).is_err() || stop {
                    return;
                }
            }
        })
        .context("cannot start relay subscription reader")?;
    Ok(())
}

fn spawn_output(
    output: Receiver<ServerMessage>,
    events: SyncSender<Event>,
) -> Result<thread::JoinHandle<Result<()>>> {
    thread::Builder::new()
        .name("herdr-hub-relay-output".into())
        .spawn(move || -> Result<()> {
            let stdout = io::stdout();
            let mut writer = BufWriter::new(stdout.lock());
            for message in output {
                if let Err(error) = write_message(&mut writer, &message) {
                    let _ = events.try_send(Event::OutputFailed(format!("{error:#}")));
                    return Err(error);
                }
            }
            Ok(())
        })
        .context("cannot start relay output writer")
}

fn send(outgoing: &SyncSender<ServerMessage>, message: ServerMessage) -> Result<()> {
    outgoing
        .send(message)
        .map_err(|_| anyhow!("relay output writer stopped"))
}

fn write_message(writer: &mut impl Write, message: &impl Serialize) -> Result<()> {
    let mut frame = serde_json::to_vec(message)?;
    if frame.len() + 1 > ndjson::MAX_FRAME_BYTES {
        bail!("Herdr Hub relay message exceeds 1 MiB")
    }
    frame.push(b'\n');
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

enum Event {
    Input(ClientMessage),
    InputClosed,
    InputFailed(String),
    CallFinished(ServerMessage),
    Local(ServerMessage),
    LocalClosed,
    LocalFailed(String),
    OutputFailed(String),
}

struct LocalFilter {
    sessions: HashSet<String>,
}

impl LocalFilter {
    fn from_model(mut model: Model) -> (Self, ServerMessage) {
        model.active = None;
        model.hosts.retain(|host| host.key == "local");
        model.sessions.retain(|session| session.host == "local");
        for session in &mut model.sessions {
            session.client_focused = None;
        }
        let sessions = model
            .sessions
            .iter()
            .map(|session| session.key.clone())
            .collect();
        (
            Self { sessions },
            ServerMessage::Hello {
                protocol: PROTOCOL,
                model,
            },
        )
    }

    fn message(&mut self, mut message: ServerMessage) -> Option<ServerMessage> {
        match &mut message {
            ServerMessage::Session { session, .. } if session.host == "local" => {
                session.client_focused = None;
                self.sessions.insert(session.key.clone());
                Some(message)
            }
            ServerMessage::SessionRemoved { key, .. } if self.sessions.remove(key) => Some(message),
            ServerMessage::Hello { .. }
            | ServerMessage::Session { .. }
            | ServerMessage::SessionRemoved { .. }
            | ServerMessage::Host { .. }
            | ServerMessage::Active { .. }
            | ServerMessage::Reply { .. }
            | ServerMessage::ReplyError { .. }
            | ServerMessage::Error { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_hub_client::{HostState, SessionState};
    use serde_json::json;

    fn session(key: &str, host: &str) -> SessionState {
        SessionState {
            key: key.into(),
            host: host.into(),
            name: key.rsplit('/').next().unwrap().into(),
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
    fn filters_initial_and_incremental_state_to_local_sessions() {
        let model = Model {
            version: 8,
            active: Some("local/default".into()),
            hosts: vec![
                HostState {
                    key: "local".into(),
                    connected: true,
                    error: None,
                },
                HostState {
                    key: "nested".into(),
                    connected: true,
                    error: None,
                },
            ],
            sessions: vec![
                session("local/default", "local"),
                session("nested/default", "nested"),
            ],
        };
        let (mut filter, hello) = LocalFilter::from_model(model);
        let ServerMessage::Hello { model, .. } = hello else {
            panic!("expected hello")
        };
        assert_eq!(model.active, None);
        assert_eq!(model.hosts.len(), 1);
        assert_eq!(model.sessions.len(), 1);
        assert_eq!(model.sessions[0].key, "local/default");
        assert_eq!(model.sessions[0].client_focused, None);

        assert!(
            filter
                .message(ServerMessage::Session {
                    version: 9,
                    session: session("nested/new", "nested"),
                })
                .is_none()
        );
        assert!(
            filter
                .message(ServerMessage::SessionRemoved {
                    version: 10,
                    key: "nested/default".into(),
                })
                .is_none()
        );
        let Some(ServerMessage::Session { session, .. }) = filter.message(ServerMessage::Session {
            version: 11,
            session: session("local/new", "local"),
        }) else {
            panic!("expected local session")
        };
        assert_eq!(session.client_focused, None);
        assert!(
            filter
                .message(ServerMessage::SessionRemoved {
                    version: 12,
                    key: "local/default".into(),
                })
                .is_some()
        );
        assert!(
            filter
                .message(ServerMessage::SessionRemoved {
                    version: 13,
                    key: "local/default".into(),
                })
                .is_none()
        );
        assert!(
            filter
                .message(ServerMessage::SessionRemoved {
                    version: 14,
                    key: "local/new".into(),
                })
                .is_some()
        );
    }

    #[test]
    fn rejects_protocol_mismatches_and_non_local_calls_without_contacting_the_hub() {
        let client = HubClient::new();
        let (events, _incoming) = mpsc::sync_channel(1);
        let wrong_protocol = start_call(
            &client,
            ClientMessage::Call {
                protocol: PROTOCOL + 1,
                id: 3,
                session: "local/default".into(),
                method: "pane.focus".into(),
                params: json!({}),
            },
            events.clone(),
            false,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            wrong_protocol,
            ServerMessage::ReplyError { id: 3, .. }
        ));

        let nested = start_call(
            &client,
            ClientMessage::Call {
                protocol: PROTOCOL,
                id: 4,
                session: "nested/default".into(),
                method: "pane.focus".into(),
                params: json!({}),
            },
            events,
            false,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(nested, ServerMessage::ReplyError { id: 4, .. }));
    }
}
