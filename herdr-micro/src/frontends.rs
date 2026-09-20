//! Direct TUI discovery. Quiet subscriptions stay live until EOF; no heartbeat.
use herdr_client::frontend::{self, FrontendClient, Route, Snapshot};
use serde::{Deserialize, Serialize};
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Ordinary input needs only the TUI; the endpoint is informational.
    pub fn input_route(&self) -> ClientRoute {
        let route = self
            .active_endpoint_id
            .as_deref()
            .and_then(|id| self.route(id));
        route.unwrap_or_else(|| ClientRoute {
            client_id: self.client_id.clone(),
            endpoint_id: self.active_endpoint_id.clone().unwrap_or_default(),
            boot_id: None,
            socket_path: self.socket_path.clone(),
        })
    }
}
#[derive(Debug, Default, Clone)]
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

/// Poll the socket directory for TUIs coming and going; each live TUI gets one
/// push subscription.
pub(crate) fn spawn_updates(
    mut on_update: impl FnMut(Update) -> bool + Send + 'static,
    stopping: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let (tx, rx) = mpsc::sync_channel(128);
        let mut workers: BTreeMap<PathBuf, (u64, Arc<AtomicBool>)> = BTreeMap::new();
        let mut snapshots = BTreeMap::new();
        let mut incarnation = 0_u64;
        let mut previous = Vec::new();
        while !stopping.load(Ordering::Acquire) {
            let paths = frontend::discover(&frontend::directory());
            workers.retain(|path, (_, stop)| {
                if paths.contains(path) {
                    true
                } else {
                    stop.store(true, Ordering::Release);
                    snapshots.remove(path);
                    false
                }
            });
            for path in paths {
                if workers.contains_key(&path) {
                    continue;
                }
                let stop = Arc::new(AtomicBool::new(false));
                incarnation += 1;
                let token = incarnation;
                workers.insert(path.clone(), (token, stop.clone()));
                let tx = tx.clone();
                let stopping = stopping.clone();
                thread::spawn(move || {
                    let mut delay = Duration::from_millis(250);
                    while !stop.load(Ordering::Acquire) && !stopping.load(Ordering::Acquire) {
                        if let Ok(mut subscription) = FrontendClient::connect(&path)
                            .with_timeout(Duration::from_secs(2))
                            .subscribe()
                        {
                            delay = Duration::from_millis(250);
                            loop {
                                if stop.load(Ordering::Acquire) || stopping.load(Ordering::Acquire)
                                {
                                    return;
                                }
                                match subscription.next_snapshot(Duration::from_millis(100)) {
                                    Ok(Some(snapshot)) => {
                                        if tx.send((path.clone(), token, Some(snapshot))).is_err() {
                                            return;
                                        }
                                    }
                                    Err(e) if e.code == "timeout" => continue,
                                    _ => break,
                                }
                            }
                        }
                        if tx.send((path.clone(), token, None)).is_err() {
                            return;
                        }
                        let until = std::time::Instant::now() + delay;
                        while std::time::Instant::now() < until
                            && !stop.load(Ordering::Acquire)
                            && !stopping.load(Ordering::Acquire)
                        {
                            thread::sleep(Duration::from_millis(50));
                        }
                        delay = (delay * 2).min(Duration::from_secs(5));
                    }
                });
            }
            // Pushed snapshots wake the loop at once; only discovery waits.
            let first = match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(message) => Some(message),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            for (path, token, snapshot) in first.into_iter().chain(rx.try_iter().take(255)) {
                if workers
                    .get(&path)
                    .is_none_or(|(current, _)| *current != token)
                {
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
            }
            let next: Vec<_> = snapshots.values().cloned().collect();
            if next != previous {
                previous = next.clone();
                if !on_update(next) {
                    break;
                }
            }
        }
        for (_, stop) in workers.values() {
            stop.store(true, Ordering::Release);
        }
    });
}
