use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use herdr_client::{Client, SessionSnapshot};
use herdr_hub_client::SessionState;

use crate::{discover::LocalSession, hub::Event};

const MIN_PROTOCOL: u32 = 20;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(5);
const FAILURE_REPORT_AFTER: Duration = Duration::from_secs(30);
const EVENT_RETRY: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub(crate) struct Update {
    pub(crate) key: String,
    pub(crate) generation: u64,
    pub(crate) state: SessionState,
}

pub(crate) struct Watcher {
    pub(crate) generation: u64,
    active: Arc<AtomicBool>,
    refresh: Arc<AtomicBool>,
    worker: thread::Thread,
    join: Option<JoinHandle<()>>,
}

impl Watcher {
    pub(crate) fn spawn(
        session: LocalSession,
        generation: u64,
        updates: SyncSender<Event>,
    ) -> Result<Self> {
        let active = Arc::new(AtomicBool::new(true));
        let refresh = Arc::new(AtomicBool::new(false));
        let worker_active = Arc::clone(&active);
        let worker_refresh = Arc::clone(&refresh);
        let join = thread::Builder::new()
            .name(format!("herdr-hub-{}", session.name))
            .spawn(move || watch(session, generation, updates, worker_active, worker_refresh))
            .context("cannot start session watcher")?;
        let worker = join.thread().clone();
        Ok(Self {
            generation,
            active,
            refresh,
            worker,
            join: Some(join),
        })
    }

    pub(crate) fn wake(&self) {
        self.refresh.store(true, Ordering::Release);
        self.worker.unpark();
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.worker.unpark();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn watch(
    session: LocalSession,
    generation: u64,
    updates: SyncSender<Event>,
    active: Arc<AtomicBool>,
    refresh: Arc<AtomicBool>,
) {
    let mut retry = SNAPSHOT_INTERVAL;
    let mut outage_started = Instant::now();
    let mut failure_reported = false;
    while active.load(Ordering::Acquire) {
        let mut connected = false;
        let mut protocol = 0;
        let result = follow(
            &session,
            generation,
            &updates,
            &active,
            &refresh,
            &mut connected,
            &mut protocol,
        );
        if !active.load(Ordering::Acquire) || result.is_ok() {
            break;
        }
        let error = result.unwrap_err();
        let incompatible = protocol != 0 && protocol < MIN_PROTOCOL;
        if connected
            || (!failure_reported
                && (incompatible || outage_started.elapsed() >= FAILURE_REPORT_AFTER))
        {
            failure_reported = true;
            if send_state(
                &updates,
                &active,
                generation,
                disconnected_state(&session, protocol, format!("{error:#}")),
            )
            .is_err()
            {
                break;
            }
        }
        if connected {
            retry = SNAPSHOT_INTERVAL;
            outage_started = Instant::now();
            failure_reported = false;
        }
        thread::park_timeout(retry);
        retry = (retry * 2).min(RECONNECT_MAX);
    }
}

fn follow(
    session: &LocalSession,
    generation: u64,
    updates: &SyncSender<Event>,
    active: &AtomicBool,
    refresh: &AtomicBool,
    connected: &mut bool,
    protocol: &mut u32,
) -> Result<()> {
    let client = Client::new(&session.socket_path).with_timeout(IO_TIMEOUT);
    let mut previous = None;
    while active.load(Ordering::Acquire) {
        let snapshot = client
            .snapshot()
            .context("cannot read Herdr session snapshot")?;
        *protocol = snapshot.protocol;
        validate_snapshot(&snapshot)?;
        let state = connected_state(session, snapshot);
        if refresh.swap(false, Ordering::AcqRel) {
            previous = None;
        }
        publish_changed(updates, active, generation, &mut previous, state)?;
        *connected = true;
        thread::park_timeout(SNAPSHOT_INTERVAL);
    }
    Ok(())
}

fn validate_snapshot(snapshot: &SessionSnapshot) -> Result<()> {
    if snapshot.protocol < MIN_PROTOCOL {
        bail!(
            "Herdr protocol {} is unsupported; expected at least {MIN_PROTOCOL}",
            snapshot.protocol
        );
    }
    if snapshot
        .panes
        .iter()
        .any(|pane| pane.pane_id.is_empty() || pane.terminal_id.is_empty())
        || snapshot
            .agents
            .iter()
            .any(|agent| agent.pane_id.is_empty() || agent.terminal_id.is_empty())
    {
        bail!("Herdr returned empty pane or terminal identity");
    }
    Ok(())
}

fn connected_state(session: &LocalSession, snapshot: SessionSnapshot) -> SessionState {
    SessionState {
        key: session.key(),
        host: "local".into(),
        name: session.name.clone(),
        connected: true,
        error: None,
        protocol: snapshot.protocol,
        workspaces: snapshot.workspaces,
        tabs: snapshot.tabs,
        agents: snapshot.agents,
        socket_path: Some(session.socket_path.clone()),
        client_focused: snapshot.client_focused,
    }
}

fn publish_changed(
    updates: &SyncSender<Event>,
    active: &AtomicBool,
    generation: u64,
    previous: &mut Option<SessionState>,
    state: SessionState,
) -> Result<()> {
    if previous.as_ref() == Some(&state) {
        return Ok(());
    }
    send_state(updates, active, generation, state.clone())?;
    *previous = Some(state);
    Ok(())
}

fn send_state(
    updates: &SyncSender<Event>,
    active: &AtomicBool,
    generation: u64,
    state: SessionState,
) -> Result<()> {
    let mut event = Event::Watch(Update {
        key: state.key.clone(),
        generation,
        state,
    });
    loop {
        if !active.load(Ordering::Acquire) {
            bail!("watcher stopped")
        }
        match updates.try_send(event) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                event = returned;
                thread::sleep(EVENT_RETRY);
            }
            Err(TrySendError::Disconnected(_)) => bail!("hub stopped"),
        }
    }
}

fn disconnected_state(session: &LocalSession, protocol: u32, error: String) -> SessionState {
    SessionState {
        key: session.key(),
        host: "local".into(),
        name: session.name.clone(),
        connected: false,
        error: Some(error),
        protocol,
        workspaces: Vec::new(),
        tabs: Vec::new(),
        agents: Vec::new(),
        socket_path: Some(session.socket_path.clone()),
        client_focused: None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn state(focused: Option<bool>) -> SessionState {
        SessionState {
            key: "local/default".into(),
            host: "local".into(),
            name: "default".into(),
            connected: true,
            error: None,
            protocol: MIN_PROTOCOL,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            agents: Vec::new(),
            socket_path: Some("/tmp/herdr.sock".into()),
            client_focused: focused,
        }
    }

    #[test]
    fn only_changed_snapshots_are_published() {
        let (updates, received) = mpsc::sync_channel(2);
        let active = AtomicBool::new(true);
        let mut previous = None;
        publish_changed(&updates, &active, 4, &mut previous, state(Some(false))).unwrap();
        publish_changed(&updates, &active, 4, &mut previous, state(Some(false))).unwrap();
        assert!(matches!(received.try_recv(), Ok(Event::Watch(_))));
        assert!(received.try_recv().is_err());

        publish_changed(&updates, &active, 4, &mut previous, state(Some(true))).unwrap();
        let Event::Watch(update) = received.try_recv().unwrap() else {
            panic!("expected watcher update")
        };
        assert_eq!(update.state.client_focused, Some(true));
    }

    #[test]
    fn cancellation_releases_a_blocked_publish() {
        let (updates, _received) = mpsc::sync_channel(0);
        let active = Arc::new(AtomicBool::new(true));
        let worker_active = Arc::clone(&active);
        let worker = thread::spawn(move || {
            send_state(&updates, &worker_active, 1, state(None))
                .unwrap_err()
                .to_string()
        });
        thread::sleep(Duration::from_millis(20));
        active.store(false, Ordering::Release);
        assert_eq!(worker.join().unwrap(), "watcher stopped");
    }

    #[test]
    fn disconnected_snapshots_cannot_claim_focus() {
        let session = LocalSession {
            name: "default".into(),
            socket_path: "/tmp/herdr.sock".into(),
        };
        assert_eq!(
            disconnected_state(&session, MIN_PROTOCOL, "offline".into()).client_focused,
            None
        );
    }
}
