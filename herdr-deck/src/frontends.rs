//! Direct TUI discovery. Quiet subscriptions stay live until EOF; no heartbeat.
use herdr_client::AgentInfo;
use herdr_frontend::{self as frontend, FrontendClient, Route, Snapshot};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

/// A captured destination: which TUI socket, which endpoint, and the server
/// boot whose pane ids were observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRoute {
    pub client_id: String,
    pub endpoint_id: String,
    pub boot_id: Option<String>,
    pub socket_path: PathBuf,
}
impl ClientRoute {
    pub fn wire(&self) -> Route {
        Route {
            endpoint_id: self.endpoint_id.clone(),
            boot_id: self.boot_id.clone(),
        }
    }
    pub fn client(&self) -> FrontendClient {
        FrontendClient::connect(&self.socket_path)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientState {
    pub socket_path: PathBuf,
    pub snapshot: Snapshot,
}
impl std::ops::Deref for ClientState {
    type Target = Snapshot;
    fn deref(&self) -> &Snapshot {
        &self.snapshot
    }
}
impl ClientState {
    pub fn route(&self, id: &str) -> Option<ClientRoute> {
        let r = self.snapshot.route(id)?;
        Some(ClientRoute {
            client_id: self.client_id.clone(),
            endpoint_id: r.endpoint_id,
            boot_id: r.boot_id,
            socket_path: self.socket_path.clone(),
        })
    }
}
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Model {
    pub clients: Vec<ClientState>,
}
impl Model {
    pub fn foremost_client(&self) -> Option<&ClientState> {
        let mut focused = self.clients.iter().filter(|c| c.focused == Some(true));
        let first = focused.next()?;
        focused.next().is_none().then_some(first)
    }
}
pub type Update = Vec<ClientState>;

pub fn project_agent(agent: &frontend::Agent, snapshot: &frontend::ShellSnapshot) -> AgentInfo {
    let pane = snapshot.panes.iter().find(|p| p.pane_id == agent.pane_id);
    AgentInfo {
        terminal_id: agent.pane_id.clone(),
        name: agent.name.clone(),
        agent: agent.agent.clone(),
        title: agent.title.clone(),
        terminal_title: agent.terminal_title.clone(),
        terminal_title_stripped: agent.terminal_title_stripped.clone(),
        display_agent: agent.display_agent.clone(),
        agent_status: agent.agent_status.as_str().into(),
        screen_detection_skipped: false,
        state_labels: agent.state_labels.iter().cloned().collect(),
        tokens: agent.tokens.iter().cloned().collect(),
        agent_session: None,
        workspace_id: agent.workspace_id.clone(),
        tab_id: agent.tab_id.clone(),
        pane_id: agent.pane_id.clone(),
        focused: agent.focused,
        launch_pending: false,
        interactive_ready: false,
        state_change_seq: agent.state_change_seq,
        cwd: pane.and_then(|p| p.cwd.clone()),
        foreground_cwd: pane.and_then(|p| p.foreground_cwd.clone()),
        revision: snapshot.revision,
    }
}

/// Deliver every observed update to the routing guard, including A→B→A.
/// Display frames may coalesce; route invalidations must not.
pub fn spawn_updates(
    directory: PathBuf,
    mut on_update: impl FnMut(Update) -> bool + Send + 'static,
    stopping: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (tx, rx) = mpsc::sync_channel(128);
        let mut workers = BTreeMap::new();
        let mut snapshots = BTreeMap::new();
        let mut next_scan = std::time::Instant::now();
        while !stopping.load(Ordering::Acquire) {
            if std::time::Instant::now() >= next_scan {
                let paths = frontend::discover(&directory);
                workers.retain(
                    |path, (stop, handle): &mut (Arc<AtomicBool>, thread::JoinHandle<()>)| {
                        if !paths.contains(path) {
                            stop.store(true, Ordering::Release);
                            snapshots.remove(path);
                        }
                        !handle.is_finished()
                    },
                );
                for path in paths {
                    if workers.contains_key(&path) {
                        continue;
                    }
                    let stop = Arc::new(AtomicBool::new(false));
                    let worker_stop = stop.clone();
                    let stopping = stopping.clone();
                    let tx = tx.clone();
                    let worker_path = path.clone();
                    let handle = thread::spawn(move || {
                        watch(worker_path, tx, worker_stop, stopping);
                    });
                    workers.insert(path, (stop, handle));
                }
                if !on_update(snapshots.values().cloned().collect()) {
                    break;
                }
                next_scan = std::time::Instant::now() + Duration::from_millis(250);
            }
            if let Ok((path, owner, snapshot)) = rx.recv_timeout(Duration::from_millis(50)) {
                // A stopped owner cannot resurrect its observations. It is retained
                // until exit, so a replacement watcher cannot race its final EOF.
                if workers.get(&path).is_none_or(|(stop, _)| {
                    !Arc::ptr_eq(stop, &owner) || stop.load(Ordering::Acquire)
                }) {
                    continue;
                }
                if let Some(snapshot) = snapshot {
                    snapshots.insert(
                        path.clone(),
                        ClientState {
                            socket_path: path,
                            snapshot,
                        },
                    );
                } else {
                    snapshots.remove(&path);
                }
                if !on_update(snapshots.values().cloned().collect()) {
                    break;
                }
            }
        }
        for (stop, _) in workers.values() {
            stop.store(true, Ordering::Release);
        }
        drop(rx); // Release workers blocked on the bounded delivery queue.
        for (_, (_, handle)) in workers {
            let _ = handle.join();
        }
    })
}

fn watch(
    path: PathBuf,
    tx: mpsc::SyncSender<(PathBuf, Arc<AtomicBool>, Option<Snapshot>)>,
    stop: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
) {
    let stopped = || stop.load(Ordering::Acquire) || stopping.load(Ordering::Acquire);
    let mut delay = Duration::from_millis(250);
    while !stopped() {
        if let Ok(mut subscription) = FrontendClient::connect(&path)
            .with_timeout(Duration::from_secs(1))
            .subscribe()
        {
            delay = Duration::from_millis(250);
            while !stopped() {
                match subscription.next_snapshot(Duration::from_millis(100)) {
                    Ok(Some(snapshot)) => {
                        if tx
                            .send((path.clone(), stop.clone(), Some(snapshot)))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(e) if e.code == "timeout" => continue,
                    _ => break,
                }
            }
        }
        if tx.send((path.clone(), stop.clone(), None)).is_err() {
            return;
        }
        let until = std::time::Instant::now() + delay;
        while !stopped() && std::time::Instant::now() < until {
            thread::sleep(Duration::from_millis(50));
        }
        delay = (delay * 2).min(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::{fs::PermissionsExt, net::UnixListener},
    };

    #[test]
    fn discovery_delivers_bursts_quiet_connections_eof_and_reconnect() {
        let dir = std::env::temp_dir().join(format!("deck-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("client.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (advance, commands) = mpsc::channel();
        let server = thread::spawn(move || {
            for round in 0..2 {
                listener.set_nonblocking(true).unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let stream = loop {
                    if let Ok((stream, _)) = listener.accept() {
                        break stream;
                    }
                    assert!(std::time::Instant::now() < deadline, "missing subscription");
                    thread::sleep(Duration::from_millis(10));
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                writeln!(
                    reader.get_mut(),
                    "{}",
                    json!({"type":"hello","protocol":frontend::PROTOCOL,"client_id":"client-a"})
                )
                .unwrap();
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&request).unwrap()["type"],
                    "subscribe"
                );
                for (revision, pane) in [(1, "p1"), (2, "p2"), (3, "p1")] {
                    let mut snapshot = crate::routing::tests::client().snapshot;
                    snapshot.revision = revision + round * 3;
                    snapshot.input_target.as_mut().unwrap().pane_id = pane.into();
                    writeln!(
                        reader.get_mut(),
                        "{}",
                        json!({"type":"snapshot","id":1,"snapshot":snapshot})
                    )
                    .unwrap();
                }
                commands.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let stopping = Arc::new(AtomicBool::new(false));
        let (updates, received) = mpsc::channel();
        let worker = spawn_updates(
            dir.clone(),
            move |clients| updates.send(clients).is_ok(),
            stopping.clone(),
        );
        let receive_revision = |revision| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let clients = received
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .unwrap();
                if clients.first().is_some_and(|c| c.revision == revision) {
                    return clients;
                }
            }
        };
        let mut routing = crate::routing::RoutingState::default();
        routing.update(receive_revision(1));
        let route = routing.capture(&[], None, None).unwrap();
        routing.update(receive_revision(2));
        routing.update(receive_revision(3));
        assert!(
            routing.validate(&route).is_err(),
            "burst must invalidate old route"
        );
        // Several read timeouts are not EOF and must not clear a quiet client.
        thread::sleep(Duration::from_millis(350));
        for clients in received.try_iter() {
            assert!(!clients.is_empty());
        }
        advance.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let clients = received
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            if clients.is_empty() {
                break;
            }
        }
        receive_revision(4);
        receive_revision(5);
        receive_revision(6);
        stopping.store(true, Ordering::Release);
        advance.send(()).unwrap();
        server.join().unwrap();
        worker.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
