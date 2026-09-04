use std::{
    collections::HashMap,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use herdr_client::Client;
use herdr_hub_client::{HostState, ServerMessage, SessionState};
use serde_json::Value;
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    iterator::Signals,
};

use crate::{
    config::{self, HostConfig},
    discover::{self, LocalSession},
    model::Store,
    remote::{self, Supervisor as RemoteSupervisor},
    server::{self, Request, Server},
    watch::{Update, Watcher},
};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
const CALL_TIMEOUT: Duration = Duration::from_secs(2);
const MAIN_EVENT_QUEUE: usize = 256;
const REMOTE_EVENT_QUEUE: usize = 128;

pub(crate) enum Event {
    Server(Request),
    Watch(Update),
    Remote(remote::Event),
    CallFinished {
        stream: UnixStream,
        id: u64,
        reply: Result<Value, String>,
    },
    Shutdown,
}

pub fn run(herdr: &Path) -> Result<()> {
    let config = config::load().context("load Herdr Hub config")?;
    let (tx, rx) = mpsc::sync_channel(MAIN_EVENT_QUEUE);
    let mut server = Server::start(tx.clone()).context("cannot start Herdr Hub server")?;
    listen_for_shutdown(tx.clone())?;
    let mut core = Core::with_hosts(tx.clone(), &config.hosts);
    let (remote_events, remote_rx) = mpsc::sync_channel(REMOTE_EVENT_QUEUE);
    forward_remote_events(remote_rx, tx.clone())?;
    let remote = RemoteSupervisor::spawn(config.hosts, remote_events)?;
    for message in core.reconcile(discover::list_sessions(herdr)?)? {
        server.broadcast(&message);
    }
    event_loop(herdr, rx, &mut core, &mut server, &remote)
}

fn event_loop(
    herdr: &Path,
    rx: Receiver<Event>,
    core: &mut Core,
    server: &mut Server,
    remote: &RemoteSupervisor,
) -> Result<()> {
    let mut reconcile_at = Instant::now() + RECONCILE_INTERVAL;
    loop {
        let timeout = reconcile_at.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(Event::Watch(update)) => {
                for message in core.apply(update) {
                    server.broadcast(&message);
                }
            }
            Ok(Event::Remote(update)) => {
                let applied = core.apply_remote(update);
                for message in applied.messages {
                    server.broadcast(&message);
                }
                for reply in applied.replies {
                    server::reply_call(reply.stream, reply.id, reply.result);
                }
            }
            Ok(Event::Server(Request::Subscribe { id, stream })) => {
                if let Err(error) = server.add_subscriber(id, stream, core.model.get()) {
                    server::log_error(&format!("cannot add subscriber: {error:#}"));
                }
            }
            Ok(Event::Server(Request::Unsubscribe { id })) => {
                server.remove_subscriber(id);
            }
            Ok(Event::Server(Request::Call {
                stream,
                id,
                session,
                method,
                params,
            })) => match core.route(&session) {
                Some(Route::Local(socket_path)) => {
                    start_call(
                        core.tx.clone(),
                        stream,
                        id,
                        Some(socket_path),
                        method,
                        params,
                    );
                }
                Some(Route::Remote { host, epoch }) => {
                    let token = core.track_remote_call(host.clone(), epoch, stream, id);
                    if let Err(error) = remote.call(&host, epoch, token, &session, method, params)
                        && let Some(reply) = core.take_remote_call(token)
                    {
                        server::reply_call(reply.stream, reply.id, Err(error.to_string()));
                    }
                }
                None => server::reply_call(stream, id, Err("unknown session".into())),
            },
            Ok(Event::Server(Request::Notify { socket_path, event })) => {
                if event.is_some() {
                    if let Err(error) = core.notify(socket_path) {
                        server::log_error(&format!("notification failed: {error:#}"));
                    }
                } else {
                    reconcile_at = Instant::now() + RECONCILE_INTERVAL;
                    reconcile_and_broadcast(herdr, core, server);
                }
            }
            Ok(Event::CallFinished { stream, id, reply }) => {
                server::reply_call(stream, id, reply);
            }
            Ok(Event::Shutdown) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {
                reconcile_at = Instant::now() + RECONCILE_INTERVAL;
                reconcile_and_broadcast(herdr, core, server);
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn listen_for_shutdown(tx: SyncSender<Event>) -> Result<()> {
    let mut signals = Signals::new([SIGINT, SIGTERM]).context("cannot install signal handlers")?;
    thread::Builder::new()
        .name("herdr-hub-signals".into())
        .spawn(move || {
            if signals.forever().next().is_some() {
                let _ = tx.send(Event::Shutdown);
            }
        })
        .context("cannot start signal handler")?;
    Ok(())
}

fn forward_remote_events(rx: Receiver<remote::Event>, tx: SyncSender<Event>) -> Result<()> {
    thread::Builder::new()
        .name("herdr-hub-remote-events".into())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                if tx.send(Event::Remote(event)).is_err() {
                    return;
                }
            }
        })
        .context("cannot start remote event bridge")?;
    Ok(())
}

fn reconcile_and_broadcast(herdr: &Path, core: &mut Core, server: &mut Server) {
    let result = discover::list_sessions(herdr).and_then(|sessions| core.reconcile(sessions));
    match result {
        Ok(messages) => {
            for message in messages {
                server.broadcast(&message);
            }
        }
        Err(error) => server::log_error(&format!("session reconciliation failed: {error:#}")),
    }
}

fn start_call(
    tx: SyncSender<Event>,
    stream: UnixStream,
    id: u64,
    socket_path: Option<PathBuf>,
    method: String,
    params: Value,
) {
    let Some(socket_path) = socket_path else {
        server::reply_call(stream, id, Err("unknown session".into()));
        return;
    };
    thread::spawn(move || {
        let reply = Client::new(socket_path)
            .with_timeout(CALL_TIMEOUT)
            .call_value(&method, &params)
            .map_err(|error| error.to_string());
        let _ = tx.send(Event::CallFinished { stream, id, reply });
    });
}

struct WatcherEntry {
    session: LocalSession,
    connected: bool,
    listed: bool,
    watcher: Watcher,
}

enum Route {
    Local(PathBuf),
    Remote { host: String, epoch: u64 },
}

struct PendingRemoteCall {
    host: String,
    epoch: u64,
    stream: UnixStream,
    id: u64,
}

struct RemoteReply {
    stream: UnixStream,
    id: u64,
    result: Result<Value, String>,
}

#[derive(Default)]
struct RemoteApplied {
    messages: Vec<ServerMessage>,
    replies: Vec<RemoteReply>,
}

struct Core {
    model: Store,
    watchers: HashMap<String, WatcherEntry>,
    remote_epochs: HashMap<String, u64>,
    pending_remote: HashMap<u64, PendingRemoteCall>,
    next_remote_token: u64,
    next_generation: u64,
    tx: SyncSender<Event>,
}

impl Core {
    #[cfg(test)]
    fn new(tx: SyncSender<Event>) -> Self {
        Self::with_hosts(tx, &[])
    }

    fn with_hosts(tx: SyncSender<Event>, hosts: &[HostConfig]) -> Self {
        let mut model = Store::default();
        let remote_epochs = hosts
            .iter()
            .map(|host| {
                model.set_host(HostState {
                    key: host.key.clone(),
                    connected: false,
                    error: Some("connecting".into()),
                });
                (host.key.clone(), 0)
            })
            .collect();
        Self {
            model,
            watchers: HashMap::new(),
            remote_epochs,
            pending_remote: HashMap::new(),
            next_remote_token: 1,
            next_generation: 1,
            tx,
        }
    }

    fn reconcile(&mut self, sessions: Vec<LocalSession>) -> Result<Vec<ServerMessage>> {
        let mut messages = Vec::new();
        for entry in self.watchers.values_mut() {
            entry.listed = false;
        }
        for session in sessions {
            let key = session.key();
            let unchanged = self.watchers.get_mut(&key).is_some_and(|entry| {
                entry.listed = true;
                entry.session.socket_path == session.socket_path
            });
            if unchanged {
                continue;
            }
            let replacement = self.watcher_entry(session)?;
            if self.watchers.insert(key.clone(), replacement).is_some() {
                messages.extend(self.remove_session(&key));
            }
        }

        let removed: Vec<_> = self
            .watchers
            .iter()
            .filter(|(_, entry)| !entry.listed && !entry.connected)
            .map(|(key, _)| key.clone())
            .collect();
        for key in removed {
            self.watchers.remove(&key);
            messages.extend(self.remove_session(&key));
        }
        Ok(messages)
    }

    fn notify(&mut self, socket_path: PathBuf) -> Result<()> {
        if !socket_path.is_absolute() {
            return Ok(());
        }
        let Some(name) = discover::session_name(&socket_path) else {
            return Ok(());
        };
        let session = LocalSession { name, socket_path };
        let key = session.key();
        if let Some(entry) = self.watchers.get_mut(&key) {
            if entry.session.socket_path == session.socket_path {
                entry.listed = true;
                entry.watcher.wake();
            }
        } else {
            self.add(session)?;
        }
        Ok(())
    }

    fn apply(&mut self, update: Update) -> Vec<ServerMessage> {
        let Some(entry) = self.watchers.get_mut(&update.key) else {
            return Vec::new();
        };
        if entry.watcher.generation != update.generation {
            return Vec::new();
        }
        entry.connected = update.state.connected;
        if !entry.connected && !entry.listed {
            self.watchers.remove(&update.key);
            return self.remove_session(&update.key);
        }
        let previous = self.model.session(&update.key).cloned();
        let initial = previous.as_ref().is_none_or(|session| !session.connected);
        let gained_focus = update.state.connected
            && update.state.client_focused == Some(true)
            && previous
                .as_ref()
                .is_none_or(|session| session.client_focused != Some(true));
        let lost_active = self.model.get().active.as_deref() == Some(update.key.as_str())
            && (!update.state.connected || update.state.client_focused != Some(true));
        let mut messages = Vec::new();
        if let Some(message) = self.model.publish_session(update.state) {
            messages.push(message);
        }
        let next_active = if gained_focus && !initial {
            Some(update.key)
        } else if lost_active || self.model.get().active.is_none() || (gained_focus && initial) {
            self.sole_focused()
        } else {
            self.model.get().active.clone()
        };
        if let Some(message) = self.model.set_active(next_active) {
            messages.push(message);
        }
        messages
    }

    fn apply_remote(&mut self, update: remote::Event) -> RemoteApplied {
        let mut applied = RemoteApplied::default();
        match update {
            remote::Event::Connected {
                host,
                epoch,
                sessions,
            } => {
                let Some(current) = self.remote_epochs.get(&host).copied() else {
                    return applied;
                };
                if epoch <= current {
                    return applied;
                }
                applied.replies.extend(self.fail_remote_calls(
                    &host,
                    "remote connection changed before the call completed",
                ));
                applied
                    .messages
                    .extend(self.model.remove_host_sessions(&host));
                self.remote_epochs.insert(host.clone(), epoch);
                if let Some(message) = self.model.set_host(HostState {
                    key: host.clone(),
                    connected: true,
                    error: None,
                }) {
                    applied.messages.push(message);
                }
                for mut session in sessions {
                    if valid_remote_session(&host, &session) {
                        session.client_focused = None;
                        applied.messages.extend(self.model.publish_session(session));
                    }
                }
            }
            remote::Event::Session {
                host,
                epoch,
                mut session,
            } => {
                if self.remote_is_current(&host, epoch) && valid_remote_session(&host, &session) {
                    session.client_focused = None;
                    applied.messages.extend(self.model.publish_session(session));
                }
            }
            remote::Event::SessionRemoved { host, epoch, key } => {
                if self.remote_is_current(&host, epoch)
                    && valid_remote_key(&host, &key).is_some()
                    && let Some(message) = self.model.remove_session(&key)
                {
                    applied.messages.push(message);
                }
            }
            remote::Event::Disconnected { host, epoch, error } => {
                let Some(current) = self.remote_epochs.get(&host).copied() else {
                    return applied;
                };
                if epoch < current {
                    return applied;
                }
                self.remote_epochs.insert(host.clone(), epoch);
                if let Some(message) = self.model.set_host(HostState {
                    key: host.clone(),
                    connected: false,
                    error: Some(error.clone()),
                }) {
                    applied.messages.push(message);
                }
                applied
                    .messages
                    .extend(self.model.remove_host_sessions(&host));
                applied
                    .replies
                    .extend(self.fail_remote_calls(&host, &error));
            }
            remote::Event::Reply {
                host,
                epoch,
                token,
                result,
            } => {
                let matches = self
                    .pending_remote
                    .get(&token)
                    .is_some_and(|pending| pending.host == host && pending.epoch == epoch);
                if matches && let Some(pending) = self.pending_remote.remove(&token) {
                    applied.replies.push(RemoteReply {
                        stream: pending.stream,
                        id: pending.id,
                        result,
                    });
                }
            }
        }
        applied
    }

    fn route(&self, key: &str) -> Option<Route> {
        let session = self.model.session(key)?;
        if session.host == "local" {
            return self.socket_path(key).map(Route::Local);
        }
        let host = self
            .model
            .get()
            .hosts
            .iter()
            .find(|host| host.key == session.host && host.connected)?;
        let epoch = self
            .remote_epochs
            .get(&host.key)
            .copied()
            .filter(|epoch| *epoch > 0)?;
        Some(Route::Remote {
            host: host.key.clone(),
            epoch,
        })
    }

    fn track_remote_call(&mut self, host: String, epoch: u64, stream: UnixStream, id: u64) -> u64 {
        let token = self.next_remote_token;
        self.next_remote_token = self
            .next_remote_token
            .checked_add(1)
            .expect("remote call token overflowed");
        self.pending_remote.insert(
            token,
            PendingRemoteCall {
                host,
                epoch,
                stream,
                id,
            },
        );
        token
    }

    fn take_remote_call(&mut self, token: u64) -> Option<PendingRemoteCall> {
        self.pending_remote.remove(&token)
    }

    fn fail_remote_calls(&mut self, host: &str, error: &str) -> Vec<RemoteReply> {
        let tokens = self
            .pending_remote
            .iter()
            .filter(|(_, pending)| pending.host == host)
            .map(|(token, _)| *token)
            .collect::<Vec<_>>();
        tokens
            .into_iter()
            .filter_map(|token| self.pending_remote.remove(&token))
            .map(|pending| RemoteReply {
                stream: pending.stream,
                id: pending.id,
                result: Err(error.into()),
            })
            .collect()
    }

    fn remote_is_current(&self, host: &str, epoch: u64) -> bool {
        self.remote_epochs.get(host) == Some(&epoch)
            && self
                .model
                .get()
                .hosts
                .iter()
                .any(|state| state.key == host && state.connected)
    }

    fn focused_local_sessions(&self) -> Vec<String> {
        self.model
            .get()
            .sessions
            .iter()
            .filter(|session| {
                session.host == "local" && session.connected && session.client_focused == Some(true)
            })
            .map(|session| session.key.clone())
            .collect()
    }

    fn sole_other_focused(&self, key: &str) -> Option<String> {
        let mut focused = self
            .focused_local_sessions()
            .into_iter()
            .filter(|candidate| candidate != key);
        let only = focused.next()?;
        focused.next().is_none().then_some(only)
    }

    fn sole_focused(&self) -> Option<String> {
        let mut focused = self.focused_local_sessions().into_iter();
        let only = focused.next()?;
        focused.next().is_none().then_some(only)
    }

    fn remove_session(&mut self, key: &str) -> Vec<ServerMessage> {
        let mut messages = Vec::new();
        if (self.model.get().active.as_deref() == Some(key) || self.model.get().active.is_none())
            && let Some(message) = self.model.set_active(self.sole_other_focused(key))
        {
            messages.push(message);
        }
        if let Some(message) = self.model.remove_session(key) {
            messages.push(message);
        }
        messages
    }

    fn socket_path(&self, key: &str) -> Option<PathBuf> {
        self.watchers
            .get(key)
            .map(|entry| entry.session.socket_path.clone())
    }

    fn add(&mut self, session: LocalSession) -> Result<()> {
        let key = session.key();
        let entry = self.watcher_entry(session)?;
        self.watchers.insert(key, entry);
        Ok(())
    }

    fn watcher_entry(&mut self, session: LocalSession) -> Result<WatcherEntry> {
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("watcher generation overflowed");
        let watcher = Watcher::spawn(session.clone(), generation, self.tx.clone())?;
        Ok(WatcherEntry {
            session,
            connected: false,
            listed: true,
            watcher,
        })
    }
}

fn valid_remote_key<'a>(host: &str, key: &'a str) -> Option<&'a str> {
    let name = key.strip_prefix(&format!("{host}/"))?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

fn valid_remote_session(host: &str, session: &SessionState) -> bool {
    session.host == host
        && session.socket_path.is_none()
        && valid_remote_key(host, &session.key) == Some(session.name.as_str())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
    };

    use super::*;
    use herdr_hub_client::SessionState;
    use serde_json::json;

    fn session(name: &str) -> LocalSession {
        LocalSession {
            name: name.into(),
            socket_path: format!("/tmp/herdr/sessions/{name}/herdr.sock").into(),
        }
    }

    fn state(name: &str, connected: bool) -> SessionState {
        SessionState {
            key: format!("local/{name}"),
            host: "local".into(),
            name: name.into(),
            connected,
            error: (!connected).then(|| "offline".into()),
            protocol: 20,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            agents: Vec::new(),
            socket_path: Some(format!("/tmp/herdr/sessions/{name}/herdr.sock").into()),
            client_focused: None,
        }
    }

    #[test]
    fn stale_generation_cannot_overwrite_replacement() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut core = Core::new(tx);
        core.add(session("work")).unwrap();
        let generation = core.watchers["local/work"].watcher.generation;
        assert!(
            core.apply(Update {
                key: "local/work".into(),
                generation: generation + 1,
                state: state("work", true),
            })
            .is_empty()
        );
        assert!(core.model.get().sessions.is_empty());
        assert!(
            core.apply(Update {
                key: "local/work".into(),
                generation,
                state: state("work", true),
            })
            .len()
                == 1
        );
    }

    #[test]
    fn reconciliation_adds_missed_session_and_removes_it_after_eof() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut core = Core::new(tx);
        assert!(core.reconcile(vec![session("work")]).unwrap().is_empty());
        assert!(core.watchers.contains_key("local/work"));
        let generation = core.watchers["local/work"].watcher.generation;
        core.apply(Update {
            key: "local/work".into(),
            generation,
            state: state("work", true),
        });
        assert!(core.reconcile(Vec::new()).unwrap().is_empty());
        assert!(core.watchers.contains_key("local/work"));
        let messages = core.apply(Update {
            key: "local/work".into(),
            generation,
            state: state("work", false),
        });
        assert!(!core.watchers.contains_key("local/work"));
        assert!(messages.iter().any(
            |message| matches!(message, ServerMessage::SessionRemoved { key, .. } if key == "local/work")
        ));
    }

    fn apply_focus(core: &mut Core, name: &str, focused: Option<bool>) -> Vec<ServerMessage> {
        if !core.watchers.contains_key(&format!("local/{name}")) {
            core.add(session(name)).unwrap();
        }
        let key = format!("local/{name}");
        let generation = core.watchers[&key].watcher.generation;
        let mut update = state(name, true);
        update.client_focused = focused;
        core.apply(Update {
            key,
            generation,
            state: update,
        })
    }

    #[test]
    fn fresh_focus_gain_claims_active_and_stale_true_does_not_steal_it_back() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut core = Core::new(tx);
        apply_focus(&mut core, "a", Some(true));
        apply_focus(&mut core, "b", Some(false));
        assert_eq!(core.model.get().active.as_deref(), Some("local/a"));

        apply_focus(&mut core, "b", Some(true));
        assert_eq!(core.model.get().active.as_deref(), Some("local/b"));
        apply_focus(&mut core, "a", Some(true));
        assert_eq!(core.model.get().active.as_deref(), Some("local/b"));
    }

    #[test]
    fn initial_focus_ambiguity_fails_closed() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut core = Core::new(tx);
        apply_focus(&mut core, "a", Some(true));
        apply_focus(&mut core, "b", Some(true));
        assert_eq!(core.model.get().active, None);
    }

    #[test]
    fn ambiguity_recovers_when_one_focus_claim_is_lost() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut core = Core::new(tx);
        apply_focus(&mut core, "a", Some(true));
        assert_eq!(core.model.get().active.as_deref(), Some("local/a"));
        apply_focus(&mut core, "b", Some(true));
        assert_eq!(core.model.get().active, None);

        apply_focus(&mut core, "a", Some(false));
        assert_eq!(core.model.get().active.as_deref(), Some("local/b"));
    }

    #[test]
    fn ambiguity_recovers_when_one_focus_claim_is_removed() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut core = Core::new(tx);
        apply_focus(&mut core, "a", Some(true));
        apply_focus(&mut core, "b", Some(true));
        assert_eq!(core.model.get().active, None);

        let messages = core.remove_session("local/a");
        assert!(matches!(
            messages.as_slice(),
            [
                ServerMessage::Active {
                    key: Some(active),
                    ..
                },
                ServerMessage::SessionRemoved { key, .. }
            ] if active == "local/b" && key == "local/a"
        ));
        assert_eq!(core.model.get().active.as_deref(), Some("local/b"));
    }

    #[test]
    fn active_loss_disconnect_and_remove_use_only_a_sole_focused_fallback() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut core = Core::new(tx);
        apply_focus(&mut core, "a", Some(true));
        apply_focus(&mut core, "b", Some(false));
        apply_focus(&mut core, "b", Some(true));

        apply_focus(&mut core, "b", Some(false));
        assert_eq!(core.model.get().active.as_deref(), Some("local/a"));

        apply_focus(&mut core, "b", Some(true));
        let key = "local/b";
        let generation = core.watchers[key].watcher.generation;
        let messages = core.apply(Update {
            key: key.into(),
            generation,
            state: state("b", false),
        });
        assert_eq!(core.model.get().active.as_deref(), Some("local/a"));
        assert!(messages.iter().any(|message| matches!(message, ServerMessage::Active { key: Some(key), .. } if key == "local/a")));

        apply_focus(&mut core, "b", Some(false));
        apply_focus(&mut core, "b", Some(true));
        core.remove_session("local/b");
        assert_eq!(core.model.get().active.as_deref(), Some("local/a"));

        apply_focus(&mut core, "c", Some(true));
        apply_focus(&mut core, "b", Some(false));
        apply_focus(&mut core, "b", Some(true));
        apply_focus(&mut core, "b", Some(false));
        assert_eq!(core.model.get().active, None);
    }

    #[test]
    fn reconnecting_into_ambiguous_focus_fails_closed() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut core = Core::new(tx);
        apply_focus(&mut core, "a", Some(true));
        apply_focus(&mut core, "b", Some(false));
        let key = "local/b";
        let generation = core.watchers[key].watcher.generation;
        core.apply(Update {
            key: key.into(),
            generation,
            state: state("b", false),
        });
        apply_focus(&mut core, "b", Some(true));
        assert_eq!(core.model.get().active, None);
    }

    #[test]
    fn remote_epoch_owns_host_sessions_and_call_replies() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let hosts = [HostConfig {
            key: "workbox".into(),
            ssh: "workbox".into(),
        }];
        let mut core = Core::with_hosts(tx, &hosts);
        assert!(core.model.get().hosts.iter().any(|host| {
            host.key == "workbox" && !host.connected && host.error.as_deref() == Some("connecting")
        }));

        let initial_failure = core.apply_remote(remote::Event::Disconnected {
            host: "workbox".into(),
            epoch: 1,
            error: "ssh unavailable".into(),
        });
        assert!(initial_failure.replies.is_empty());
        assert!(matches!(
            initial_failure.messages.as_slice(),
            [ServerMessage::Host { host, .. }]
                if !host.connected && host.error.as_deref() == Some("ssh unavailable")
        ));

        let mut session = state("default", true);
        session.key = "workbox/default".into();
        session.host = "workbox".into();
        session.socket_path = None;
        session.client_focused = Some(true);
        let connected = core.apply_remote(remote::Event::Connected {
            host: "workbox".into(),
            epoch: 2,
            sessions: vec![session],
        });
        assert!(connected.replies.is_empty());
        assert_eq!(core.model.get().sessions.len(), 1);
        assert_eq!(core.model.get().sessions[0].client_focused, None);
        assert!(matches!(
            core.route("workbox/default"),
            Some(Route::Remote { ref host, epoch: 2 }) if host == "workbox"
        ));

        let (stream, _peer) = UnixStream::pair().unwrap();
        let token = core.track_remote_call("workbox".into(), 2, stream, 73);
        assert!(
            core.apply_remote(remote::Event::Reply {
                host: "workbox".into(),
                epoch: 1,
                token,
                result: Ok(json!({"stale": true})),
            })
            .replies
            .is_empty()
        );
        let reply = core.apply_remote(remote::Event::Reply {
            host: "workbox".into(),
            epoch: 2,
            token,
            result: Ok(json!({"focused": true})),
        });
        assert_eq!(reply.replies.len(), 1);
        assert_eq!(reply.replies[0].id, 73);
        assert_eq!(reply.replies[0].result, Ok(json!({"focused": true})));

        let disconnected = core.apply_remote(remote::Event::Disconnected {
            host: "workbox".into(),
            epoch: 2,
            error: "ssh lost".into(),
        });
        assert!(disconnected.replies.is_empty());
        assert!(core.model.get().sessions.is_empty());
        assert!(core.route("workbox/default").is_none());
        assert!(core.model.get().hosts.iter().any(|host| {
            host.key == "workbox" && !host.connected && host.error.as_deref() == Some("ssh lost")
        }));
    }

    #[test]
    fn remote_updates_fail_closed_for_stale_or_malformed_sessions() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let hosts = [HostConfig {
            key: "workbox".into(),
            ssh: "workbox".into(),
        }];
        let mut core = Core::with_hosts(tx, &hosts);
        core.apply_remote(remote::Event::Connected {
            host: "workbox".into(),
            epoch: 3,
            sessions: Vec::new(),
        });

        let mut nested = state("hidden", true);
        nested.key = "nested/hidden".into();
        nested.host = "nested".into();
        nested.socket_path = None;
        assert!(
            core.apply_remote(remote::Event::Session {
                host: "workbox".into(),
                epoch: 3,
                session: nested,
            })
            .messages
            .is_empty()
        );

        let mut stale = state("old", true);
        stale.key = "workbox/old".into();
        stale.host = "workbox".into();
        stale.socket_path = None;
        assert!(
            core.apply_remote(remote::Event::Session {
                host: "workbox".into(),
                epoch: 2,
                session: stale,
            })
            .messages
            .is_empty()
        );
        assert!(core.model.get().sessions.is_empty());
    }

    #[test]
    fn custom_notification_path_is_ignored() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut core = Core::new(tx);
        core.notify("/tmp/custom.sock".into()).unwrap();
        core.notify("/tmp/herdr/herdr.sock".into()).unwrap();
        assert!(core.watchers.is_empty());
    }

    #[test]
    fn calls_are_forwarded_to_the_session_socket() {
        let path = PathBuf::from(format!("/tmp/hhc-{}.sock", std::process::id()));
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let session_server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["method"], "pane.focus");
            assert_eq!(request["params"], json!({"pane_id": "w1:p1"}));
            writeln!(
                stream,
                "{}",
                json!({
                    "id": request["id"],
                    "result": {"focused": true}
                })
            )
            .unwrap();
        });

        let (reply_stream, mut client) = UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::sync_channel(64);
        start_call(
            tx,
            reply_stream,
            17,
            Some(path.clone()),
            "pane.focus".into(),
            json!({"pane_id": "w1:p1"}),
        );
        let Event::CallFinished { stream, id, reply } =
            rx.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("expected call completion")
        };
        assert_eq!(reply.as_ref().unwrap(), &json!({"focused": true}));
        server::reply_call(stream, id, reply);
        let mut line = String::new();
        BufReader::new(&mut client).read_line(&mut line).unwrap();
        let message: ServerMessage = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            message,
            ServerMessage::Reply { id: 17, result } if result == json!({"focused": true})
        ));

        session_server.join().unwrap();
        fs::remove_file(path).unwrap();
    }
}
