//! Following the attached clients of every local Herdr daemon.
//!
//! A user usually runs several sessions, each its own daemon and socket, and
//! a client attaches to one of them. [`spawn`] rediscovers the sessions on a
//! thread of its own, keeps one `client.changed` subscription per daemon, and
//! publishes the merged [`Directory`] whenever it changes. `client.changed`
//! only names a client, so each daemon's `client.list` is read once after
//! its subscription starts and once per burst of changes; nothing polls it.
//!
//! A long-lived tool acts on [`Directory::followed_client`]: the client,
//! across all daemons, whose window most recently took focus. It stays
//! followed while another app is in front, until it detaches or another
//! client takes focus. [`Directory`] states the rule in full.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    CLIENT_JUMP_TIMEOUT, Client, ClientChanged, ClientInfo, ClientJumpError, ClientJumpParams,
    ClientList, ClientReport, ClientReportAgent, Error, PendingJump, Subscription,
    sessions::LocalSession,
};

/// Where one daemon's client inventory stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonState {
    /// Subscribed; `clients` is current. `boot_id` is the daemon's own boot.
    Following { boot_id: String },
    /// The watch is down: not connected yet, or the last subscription,
    /// listing or event failed. `clients` is empty until the next listing.
    Offline,
}

/// One followed daemon and its attached clients, lowest client id first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonClients {
    pub session: String,
    pub socket_path: PathBuf,
    pub state: DaemonState,
    pub clients: Vec<ClientInfo>,
}

/// Every followed daemon, ordered by socket path, and the client a tool
/// follows.
///
/// The followed client is chosen by one rule:
///
/// - A client whose report says `focused: true`, and did not in its daemon's
///   listing before, becomes the followed one. Listings are taken one at a
///   time, so the latest wins; of several clients in one listing, the one
///   with the highest client id.
/// - The followed client is forgotten when a listing of its daemon no
///   longer contains it (it detached, or the daemon restarted under another
///   boot) or when its daemon is dropped because a session discovery no
///   longer lists it. While its daemon is only [`DaemonState::Offline`] it
///   is kept, so one failed listing does not lose it, but
///   [`Self::followed_client`] is `None` until the daemon lists it again.
/// - While no client is followed, a reporting client is followed anyway,
///   each time a directory is published: one that reports `focused: true`
///   before one whose focus is unknown before one that reports `false`, and
///   of equals the last by socket path and then client id. It then stays
///   followed like any other.
///
/// Build one with [`Directory::updated`], so `followed` obeys the rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Directory {
    pub daemons: Vec<DaemonClients>,
    /// The identity of the followed client. [`Self::followed_client`] is
    /// the client itself, while its daemon supplies it.
    pub followed: Option<ClientKey>,
    /// The clients that reported `focused: true` in the last listing of
    /// their daemon. Kept while a daemon is offline, so the first listing
    /// after a reconnect is compared with the last one before it and a
    /// client that had focus all along does not count as taking it.
    focused: Vec<ClientKey>,
}

impl Directory {
    /// The client to show and to jump, by the rule on [`Directory`]. It
    /// stays followed when its window loses focus, so another app in front
    /// changes nothing; [`FollowedClient::has_focus`] tells the two apart.
    /// `None` while no attached client has sent a report, and while the
    /// followed client's daemon is offline: nothing is known about the
    /// client then, its focus least of all.
    pub fn followed_client(&self) -> Option<FollowedClient<'_>> {
        let key = self.followed.as_ref()?;
        self.daemons.iter().find_map(|daemon| {
            let DaemonState::Following { boot_id } = &daemon.state else {
                return None;
            };
            if daemon.socket_path != key.socket_path || *boot_id != key.boot_id {
                return None;
            }
            let client = daemon
                .clients
                .iter()
                .find(|client| client.client_id == key.client_id)?;
            Some(FollowedClient {
                session: &daemon.session,
                socket_path: &daemon.socket_path,
                boot_id,
                client_id: client.client_id,
                report: client.report.as_ref()?,
            })
        })
    }

    /// This directory after one reading of every daemon, published once:
    /// those missing from `daemons` are dropped, and the rest are observed
    /// in socket path order, each as the follower observes one listing.
    pub fn updated(&self, mut daemons: Vec<DaemonClients>) -> Self {
        let mut next = self.clone();
        let dropped: Vec<PathBuf> = next
            .daemons
            .iter()
            .map(|known| &known.socket_path)
            .filter(|path| daemons.iter().all(|daemon| daemon.socket_path != **path))
            .cloned()
            .collect();
        for path in dropped {
            next.remove(&path);
        }
        daemons.sort_by(|left, right| left.socket_path.cmp(&right.socket_path));
        for daemon in daemons {
            next.add(&daemon.session, &daemon.socket_path);
            let observation = match daemon.state {
                DaemonState::Following { boot_id } => Observation::Listed(ClientList {
                    boot_id,
                    clients: daemon.clients,
                }),
                DaemonState::Offline => Observation::Offline,
            };
            next.observe(&daemon.socket_path, observation);
        }
        next.settle();
        next
    }

    /// Attached clients across all daemons, reporting or not.
    pub fn client_count(&self) -> usize {
        self.daemons.iter().map(|daemon| daemon.clients.len()).sum()
    }

    /// Starts following the daemon at `socket_path`, offline until its
    /// first listing. A daemon already followed keeps its view.
    fn add(&mut self, session: &str, socket_path: &Path) {
        match self
            .daemons
            .binary_search_by(|daemon| daemon.socket_path.as_path().cmp(socket_path))
        {
            Ok(index) => session.clone_into(&mut self.daemons[index].session),
            Err(index) => self.daemons.insert(
                index,
                DaemonClients {
                    session: session.to_owned(),
                    socket_path: socket_path.to_owned(),
                    state: DaemonState::Offline,
                    clients: Vec::new(),
                },
            ),
        }
    }

    /// Drops the daemon at `socket_path`, and the followed client with it
    /// when it was attached there: a successful discovery no longer lists
    /// the session, so nothing accepts connections on its socket.
    fn remove(&mut self, socket_path: &Path) {
        self.daemons
            .retain(|daemon| daemon.socket_path != socket_path);
        self.focused.retain(|key| key.socket_path != socket_path);
        if self
            .followed
            .as_ref()
            .is_some_and(|followed| followed.socket_path == socket_path)
        {
            self.followed = None;
        }
    }

    /// Replaces the view of the daemon at `socket_path` with `observation`:
    /// a listing may forget the followed client, and may make a client
    /// that took focus the followed one. Returns whether the view changed.
    fn observe(&mut self, socket_path: &Path, observation: Observation) -> bool {
        let Some(daemon) = self
            .daemons
            .iter_mut()
            .find(|daemon| daemon.socket_path == socket_path)
        else {
            return false;
        };
        let (state, clients) = match observation {
            Observation::Listed(list) => {
                let mut clients = list.clients;
                clients.sort_by_key(|client| client.client_id);
                (
                    DaemonState::Following {
                        boot_id: list.boot_id,
                    },
                    clients,
                )
            }
            // The followed client is kept: this is a failed watch, not a
            // listing without it.
            Observation::Offline => (DaemonState::Offline, Vec::new()),
        };
        if daemon.state == state && daemon.clients == clients {
            return false;
        }
        daemon.state = state;
        daemon.clients = clients;
        if let DaemonState::Following { boot_id } = &daemon.state {
            let listed = |client_id| ClientKey {
                socket_path: socket_path.to_owned(),
                boot_id: boot_id.clone(),
                client_id,
            };
            if self.followed.as_ref().is_some_and(|followed| {
                followed.socket_path == socket_path
                    && !daemon
                        .clients
                        .iter()
                        .any(|client| listed(client.client_id) == *followed)
            }) {
                self.followed = None;
            }
            let focused: Vec<ClientKey> = daemon
                .clients
                .iter()
                .filter(|client| rank(client) == Some(Rank::Focused))
                .map(|client| listed(client.client_id))
                .collect();
            if let Some(gained) = focused.iter().rfind(|key| !self.focused.contains(key)) {
                self.followed = Some(gained.clone());
            }
            self.focused.retain(|key| key.socket_path != socket_path);
            self.focused.extend(focused);
        }
        true
    }

    /// While no client is followed, follows a reporting one. Run once
    /// after the daemons were dropped and observed, before the directory
    /// is read, so the pick is made among all of them.
    fn settle(&mut self) {
        if self.followed.is_some() {
            return;
        }
        self.followed = self
            .daemons
            .iter()
            .filter_map(|daemon| match &daemon.state {
                DaemonState::Following { boot_id } => Some((daemon, boot_id)),
                DaemonState::Offline => None,
            })
            .flat_map(|(daemon, boot_id)| {
                daemon.clients.iter().filter_map(move |client| {
                    Some((
                        rank(client)?,
                        &daemon.socket_path,
                        boot_id,
                        client.client_id,
                    ))
                })
            })
            .max_by_key(|(rank, socket_path, _, client_id)| (*rank, *socket_path, *client_id))
            .map(|(_, socket_path, boot_id, client_id)| ClientKey {
                socket_path: socket_path.clone(),
                boot_id: boot_id.clone(),
                client_id,
            });
    }
}

/// What a reporting client says about its window's focus, least preferred
/// first when a client has to be picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Unfocused,
    Unknown,
    Focused,
}

/// `None` for a client that has not sent a report.
fn rank(client: &ClientInfo) -> Option<Rank> {
    Some(match client.report.as_ref()?.focused {
        Some(true) => Rank::Focused,
        None => Rank::Unknown,
        Some(false) => Rank::Unfocused,
    })
}

/// The followed client and the daemon it is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowedClient<'a> {
    pub session: &'a str,
    pub socket_path: &'a Path,
    /// Boot of the daemon the client is attached to.
    pub boot_id: &'a str,
    pub client_id: u64,
    pub report: &'a ClientReport,
}

impl<'a> FollowedClient<'a> {
    /// Whether the client's window has focus right now. Keyboard events for
    /// the frontmost app reach this client only while it does.
    pub fn has_focus(&self) -> bool {
        self.report.focused == Some(true)
    }

    pub fn key(&self) -> ClientKey {
        ClientKey {
            socket_path: self.socket_path.to_owned(),
            boot_id: self.boot_id.to_owned(),
            client_id: self.client_id,
        }
    }

    /// The agent the client has focused, if any.
    pub fn focused_agent(&self) -> Option<&'a ClientReportAgent> {
        self.report.agents.iter().find(|row| row.focused)
    }

    /// Whether [`Self::jump_to`] can show `row`'s pane now: its endpoint is
    /// online and has reported its boot. A client goes on reporting the
    /// agents of an endpoint that is reconnecting, so a row that cannot be
    /// reached has not gone away.
    pub fn can_reach(&self, row: &ClientReportAgent) -> bool {
        self.report
            .endpoint(&row.endpoint_id)
            .is_some_and(|endpoint| endpoint.jump_boot_id().is_some())
    }

    /// The agents [`Self::jump_to`] can reach, in the client's display
    /// order.
    pub fn reachable_agents(&self) -> impl Iterator<Item = &'a ClientReportAgent> + use<'a> {
        let client = *self;
        client
            .report
            .agents
            .iter()
            .filter(move |row| client.can_reach(row))
    }

    /// The jump that shows `row`'s pane on this client. `None` while the
    /// row's endpoint is not online or has no boot id yet, because a jump
    /// must name the boot its pane id was read from.
    pub fn jump_to(&self, row: &ClientReportAgent) -> Option<JumpTarget> {
        let boot_id = self.report.endpoint(&row.endpoint_id)?.jump_boot_id()?;
        Some(JumpTarget {
            client: self.key(),
            endpoint_id: row.endpoint_id.clone(),
            boot_id: boot_id.to_owned(),
            pane_id: row.agent.pane_id.clone(),
        })
    }
}

/// One client connection of one daemon boot. Client ids start again when a
/// daemon restarts, so the boot is part of the identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientKey {
    pub socket_path: PathBuf,
    pub boot_id: String,
    pub client_id: u64,
}

/// A captured `client.jump`: the client to ask and the pane to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpTarget {
    pub client: ClientKey,
    pub endpoint_id: String,
    /// Boot of the endpoint the pane id was read from.
    pub boot_id: String,
    pub pane_id: String,
}

impl JumpTarget {
    pub fn params(&self) -> ClientJumpParams {
        ClientJumpParams {
            client_id: self.client.client_id,
            endpoint_id: self.endpoint_id.clone(),
            boot_id: self.boot_id.clone(),
            pane_id: self.pane_id.clone(),
        }
    }

    /// Connects to the client's daemon and writes the jump, once.
    /// [`PendingJump::wait`] then waits for the answer, so a caller can
    /// order its writes without one of them waiting for another's answer.
    pub fn start(&self) -> Result<PendingJump, ClientJumpError> {
        Client::new(&self.client.socket_path)
            .with_timeout(CLIENT_JUMP_TIMEOUT)
            .start_jump(&self.params())
    }
}

/// Reads every session's clients once, without subscribing: a diagnostic
/// view for a `doctor` command, not a way to stay current. `failed` receives
/// each daemon that could not be listed, with the reason. One reading has no
/// history, so its followed client is the one a follower would pick with
/// nothing followed.
pub fn list_once(
    sessions: &[LocalSession],
    timeout: Duration,
    mut failed: impl FnMut(&LocalSession, Error),
) -> Directory {
    let daemons = sessions
        .iter()
        .map(|session| {
            let (state, clients) = match Client::new(&session.socket_path)
                .with_timeout(timeout)
                .list_clients()
            {
                Ok(list) => (
                    DaemonState::Following {
                        boot_id: list.boot_id,
                    },
                    list.clients,
                ),
                Err(error) => {
                    failed(session, error);
                    (DaemonState::Offline, Vec::new())
                }
            };
            DaemonClients {
                session: session.name.clone(),
                socket_path: session.socket_path.clone(),
                state,
                clients,
            }
        })
        .collect();
    Directory::default().updated(daemons)
}

/// Intervals of the follower; [`Timing::default`] suits a long-lived tool.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How often the sessions are rediscovered. Discovery runs on its own
    /// thread, so a slow `herdr session list` never delays publishing.
    pub rescan: Duration,
    /// Connect, subscribe, and `client.list` timeout.
    pub io_timeout: Duration,
    /// How often an idle subscription checks whether to stop.
    pub poll: Duration,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            // How long a device keeps its previous view after a client of a
            // session started since the last scan takes focus. Each scan is one
            // short-lived process, off the publishing path.
            rescan: Duration::from_secs(3),
            io_timeout: Duration::from_secs(2),
            poll: Duration::from_millis(100),
            reconnect_min: Duration::from_millis(250),
            reconnect_max: Duration::from_secs(5),
        }
    }
}

/// Follows the sessions `discover` returns until `stopping` is set or
/// `on_update` returns false. `on_update` receives the merged directory after
/// the first discovery and then only when it changed. `log` receives the
/// lines worth keeping: a daemon connection lost, a daemon that cannot be
/// followed (once per reason), and discovery failures (once per message).
///
/// `discover` runs on a thread of its own that is never joined, so a hung
/// call cannot hold up publishing or stopping; bound it, as
/// [`crate::sessions::discover`] does.
pub fn spawn<D, E>(
    discover: D,
    on_update: impl FnMut(Directory) -> bool + Send + 'static,
    log: impl Fn(String) + Send + Sync + 'static,
    stopping: Arc<AtomicBool>,
) -> thread::JoinHandle<()>
where
    D: FnMut() -> Result<Vec<LocalSession>, E> + Send + 'static,
    E: std::fmt::Display,
{
    spawn_with(Timing::default(), discover, on_update, log, stopping)
}

/// [`spawn`] with explicit intervals.
pub fn spawn_with<D, E>(
    timing: Timing,
    discover: D,
    mut on_update: impl FnMut(Directory) -> bool + Send + 'static,
    log: impl Fn(String) + Send + Sync + 'static,
    stopping: Arc<AtomicBool>,
) -> thread::JoinHandle<()>
where
    D: FnMut() -> Result<Vec<LocalSession>, E> + Send + 'static,
    E: std::fmt::Display,
{
    let log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(log);
    let (tx, rx) = mpsc::sync_channel::<Message>(QUEUE);
    spawn_discovery(
        timing,
        discover,
        tx.clone(),
        Arc::clone(&log),
        Arc::clone(&stopping),
    );
    thread::spawn(move || {
        let mut workers: BTreeMap<PathBuf, Worker> = BTreeMap::new();
        let mut directory = Directory::default();
        let mut discovered = false;
        while !stopping.load(Ordering::Acquire) {
            let first = match rx.recv_timeout(STOP_CHECK) {
                Ok(message) => message,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            // Everything already queued is applied before one publish, each
            // listing on its own, so the latest client to take focus wins
            // within one publish too.
            let mut changed = false;
            for message in std::iter::once(first).chain(rx.try_iter().take(QUEUE)) {
                match message {
                    Message::Discovered(sessions) => {
                        changed |= !std::mem::replace(&mut discovered, true);
                        if let Some(sessions) = sessions {
                            changed |= reconcile(
                                sessions,
                                &mut workers,
                                &mut directory,
                                &tx,
                                &stopping,
                                timing,
                                &log,
                            );
                        }
                    }
                    Message::Observed {
                        path,
                        owner,
                        observation,
                    } => {
                        // A stopped or replaced worker cannot resurrect its view.
                        let current = workers.get(&path).is_some_and(|worker| {
                            Arc::ptr_eq(&worker.stop, &owner) && !owner.load(Ordering::Acquire)
                        });
                        if current {
                            changed |= directory.observe(&path, observation);
                        }
                    }
                }
            }
            if !changed {
                continue;
            }
            directory.settle();
            if !on_update(directory.clone()) {
                break;
            }
        }
        for worker in workers.values() {
            worker.stop.store(true, Ordering::Release);
        }
        drop(rx); // Release workers blocked on the bounded delivery queue.
        for (_, worker) in workers {
            let _ = worker.handle.join();
        }
    })
}

const STOP_CHECK: Duration = Duration::from_millis(50);

/// Capacity of the queue into the publishing thread, and the most messages
/// one publish takes in.
const QUEUE: usize = 128;

/// Runs `discover` now and then every `timing.rescan` after it returns,
/// until `stopping` is set or the publishing thread is gone.
fn spawn_discovery<D, E>(
    timing: Timing,
    mut discover: D,
    tx: mpsc::SyncSender<Message>,
    log: Arc<dyn Fn(String) + Send + Sync>,
    stopping: Arc<AtomicBool>,
) where
    D: FnMut() -> Result<Vec<LocalSession>, E> + Send + 'static,
    E: std::fmt::Display,
{
    thread::spawn(move || {
        let stopped = || stopping.load(Ordering::Acquire);
        let mut last_error = String::new();
        while !stopped() {
            let sessions = match discover() {
                Ok(sessions) => {
                    last_error.clear();
                    Some(sessions)
                }
                // Keep following what is already known; a missing or
                // failing herdr executable must not blank live daemons.
                Err(error) => {
                    let error = error.to_string();
                    if last_error != error {
                        log(format!("Herdr session discovery failed: {error}"));
                        last_error = error;
                    }
                    None
                }
            };
            if stopped() || tx.send(Message::Discovered(sessions)).is_err() {
                break;
            }
            pause(timing.rescan, &stopped);
        }
    });
}

struct Worker {
    stop: Arc<AtomicBool>,
    handle: thread::JoinHandle<()>,
}

/// What a worker saw of its daemon.
enum Observation {
    Listed(ClientList),
    /// The watch failed; the daemon is asked again.
    Offline,
}

enum Message {
    /// One discovery attempt: the sessions, or `None` when it failed.
    Discovered(Option<Vec<LocalSession>>),
    /// What the worker `owner` saw of the daemon at `path`.
    Observed {
        path: PathBuf,
        owner: Arc<AtomicBool>,
        observation: Observation,
    },
}

/// Starts following new sessions and stops following unlisted ones.
/// Returns whether the set of followed daemons changed.
fn reconcile(
    sessions: Vec<LocalSession>,
    workers: &mut BTreeMap<PathBuf, Worker>,
    directory: &mut Directory,
    tx: &mpsc::SyncSender<Message>,
    stopping: &Arc<AtomicBool>,
    timing: Timing,
    log: &Arc<dyn Fn(String) + Send + Sync>,
) -> bool {
    let before = workers.len();
    workers.retain(|path, worker| {
        let listed = sessions.iter().any(|session| session.socket_path == *path);
        if !listed {
            worker.stop.store(true, Ordering::Release);
            directory.remove(path);
        }
        listed
    });
    let mut changed = workers.len() != before;
    for session in sessions {
        if workers.contains_key(&session.socket_path) {
            continue;
        }
        changed = true;
        directory.add(&session.name, &session.socket_path);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = thread::spawn({
            let session = session.clone();
            let tx = tx.clone();
            let stop = Arc::clone(&stop);
            let stopping = Arc::clone(stopping);
            let log = Arc::clone(log);
            move || watch(session, tx, stop, stopping, timing, log)
        });
        workers.insert(session.socket_path, Worker { stop, handle });
    }
    changed
}

/// Follows one daemon, reconnecting with backoff, until told to stop.
fn watch(
    session: LocalSession,
    tx: mpsc::SyncSender<Message>,
    stop: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    timing: Timing,
    log: Arc<dyn Fn(String) + Send + Sync>,
) {
    let stopped = || stop.load(Ordering::Acquire) || stopping.load(Ordering::Acquire);
    let send = |observation| {
        tx.send(Message::Observed {
            path: session.socket_path.clone(),
            owner: Arc::clone(&stop),
            observation,
        })
        .is_ok()
    };
    let mut delay = timing.reconnect_min;
    // The reason last logged: a daemon that keeps failing the same way is
    // reported once, not once per attempt.
    let mut logged = String::new();
    while !stopped() {
        let mut following = false;
        let Err(error) = follow(&session, &send, &stopped, timing, &mut following) else {
            break;
        };
        let reason = error.to_string();
        if following {
            delay = timing.reconnect_min;
            log(format!(
                "Herdr session {}: lost the daemon connection: {reason}",
                session.name
            ));
        } else if logged != reason {
            log(format!(
                "Herdr session {} ({}): cannot follow its clients: {reason}",
                session.name,
                session.socket_path.display()
            ));
        }
        logged = reason;
        if !send(Observation::Offline) {
            break;
        }
        pause(delay, &stopped);
        delay = (delay * 2).min(timing.reconnect_max);
    }
}

/// Subscribes and lists until told to stop (`Ok`) or the watch fails.
/// `following` is set once the daemon was listed.
fn follow(
    session: &LocalSession,
    send: &impl Fn(Observation) -> bool,
    stopped: &impl Fn() -> bool,
    timing: Timing,
    following: &mut bool,
) -> Result<(), Error> {
    let client = Client::new(&session.socket_path).with_timeout(timing.io_timeout);
    // Subscribe before listing, so no change falls between the two.
    let mut subscription = client.subscribe_client_changes()?;
    subscription.set_receive_timeout(timing.poll)?;
    let mut listed = false;
    while !stopped() {
        if listed {
            match read_change(&mut subscription) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
            // A burst already read costs one listing, not one per change.
            while subscription.has_buffered_event() {
                read_change(&mut subscription)?;
            }
        }
        let list = client.list_clients()?;
        if !send(Observation::Listed(list)) {
            return Ok(());
        }
        listed = true;
        *following = true;
    }
    Ok(())
}

/// Reads one event: `true` for a client change, `false` for anything else.
/// An undecodable change is an error, so the caller resubscribes and lists
/// again rather than keep a stale view.
fn read_change(subscription: &mut Subscription) -> Result<bool, Error> {
    let event = subscription
        .next_event()?
        .ok_or_else(|| Error::Io(io::ErrorKind::UnexpectedEof.into()))?;
    match ClientChanged::from_event(&event) {
        Some(Ok(_)) => Ok(true),
        Some(Err(error)) => Err(Error::Json(error)),
        None => Ok(false),
    }
}

fn pause(duration: Duration, stopped: &impl Fn() -> bool) {
    let deadline = Instant::now() + duration;
    while !stopped() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        thread::sleep(left.min(STOP_CHECK));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientEndpointStatus, ClientReportEndpoint};

    fn report(focused: Option<bool>) -> ClientReport {
        ClientReport {
            focused,
            active_endpoint_id: Some("local".into()),
            endpoints: vec![ClientReportEndpoint {
                endpoint_id: "local".into(),
                label: "Local".into(),
                status: ClientEndpointStatus::ONLINE.into(),
                boot_id: Some("boot".into()),
            }],
            agents: Vec::new(),
        }
    }

    fn daemon(name: &str, clients: Vec<ClientInfo>) -> DaemonClients {
        DaemonClients {
            session: name.into(),
            socket_path: format!("/run/{name}/herdr.sock").into(),
            state: DaemonState::Following {
                boot_id: format!("boot-{name}"),
            },
            clients,
        }
    }

    fn client(client_id: u64, focused: Option<bool>) -> ClientInfo {
        ClientInfo {
            client_id,
            report: Some(report(focused)),
        }
    }

    /// An idle agent `w1:p1` of `endpoint_id`.
    fn jump_row(endpoint_id: &str) -> ClientReportAgent {
        ClientReportAgent {
            endpoint_id: endpoint_id.into(),
            focused: false,
            workspace_label: "api".into(),
            workspace_number: 1,
            cwd: None,
            foreground_cwd: None,
            agent: crate::ClientAgent {
                pane_id: "w1:p1".into(),
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
                name: None,
                display_agent: None,
                agent: None,
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: "idle".into(),
                state_change_seq: 1,
                state_labels: Vec::new(),
                tokens: Vec::new(),
                focused: false,
                priority_key: String::new(),
                priority_mark: None,
            },
        }
    }

    fn followed(directory: &Directory) -> Option<(&str, u64, bool)> {
        directory
            .followed_client()
            .map(|client| (client.session, client.client_id, client.has_focus()))
    }

    fn key(session: &str, client_id: u64) -> ClientKey {
        ClientKey {
            socket_path: format!("/run/{session}/herdr.sock").into(),
            boot_id: format!("boot-{session}"),
            client_id,
        }
    }

    /// A client that is attached and has sent no report.
    fn silent(client_id: u64) -> ClientInfo {
        ClientInfo {
            client_id,
            report: None,
        }
    }

    fn offline(name: &str) -> DaemonClients {
        DaemonClients {
            state: DaemonState::Offline,
            ..daemon(name, vec![])
        }
    }

    #[test]
    fn the_client_that_last_took_focus_is_followed_until_it_detaches() {
        // No client has sent a report: nothing is followed.
        for daemons in [
            vec![],
            vec![daemon("default", vec![])],
            vec![daemon("default", vec![silent(9)])],
            vec![offline("default")],
        ] {
            let directory = Directory::default().updated(daemons);
            assert_eq!(followed(&directory), None, "{directory:?}");
            assert_eq!(directory.followed, None);
        }

        let directory = Directory::default().updated(vec![
            daemon("default", vec![client(1, Some(false)), silent(9)]),
            daemon("work", vec![client(1, Some(true))]),
        ]);
        assert_eq!(followed(&directory), Some(("work", 1, true)));

        // Another app comes to the front: still followed, without focus.
        let directory = directory.updated(vec![
            daemon("default", vec![client(1, Some(false)), silent(9)]),
            daemon("work", vec![client(1, Some(false))]),
        ]);
        assert_eq!(followed(&directory), Some(("work", 1, false)));
        // An unchanged listing changes nothing.
        let directory = directory.updated(directory.daemons.clone());
        assert_eq!(followed(&directory), Some(("work", 1, false)));

        // A client of another daemon takes focus: it is followed from then on.
        let directory = directory.updated(vec![
            daemon("default", vec![client(1, Some(true)), silent(9)]),
            daemon("work", vec![client(1, Some(false))]),
        ]);
        assert_eq!(followed(&directory), Some(("default", 1, true)));

        // The other client claims focus too: the later claim wins, and
        // keeps winning while both still say so.
        let both = vec![
            daemon("default", vec![client(1, Some(true))]),
            daemon("work", vec![client(1, Some(true))]),
        ];
        let directory = directory.updated(both.clone());
        assert_eq!(followed(&directory), Some(("work", 1, true)));
        let directory = directory.updated(both);
        assert_eq!(followed(&directory), Some(("work", 1, true)));

        // The followed client detaches: the other one still reports, so it
        // is followed, although it took focus earlier.
        let directory = directory.updated(vec![
            daemon("default", vec![client(1, Some(true))]),
            daemon("work", vec![]),
        ]);
        assert_eq!(followed(&directory), Some(("default", 1, true)));
        // The last reporting client detaches: nothing is followed.
        let directory = directory.updated(vec![
            daemon("default", vec![silent(9)]),
            daemon("work", vec![]),
        ]);
        assert_eq!(followed(&directory), None);
        assert_eq!(directory.followed, None);
    }

    #[test]
    fn clients_that_take_focus_in_one_reading_tie_by_socket_path_then_client_id() {
        // One listing: the highest client id.
        let directory = Directory::default().updated(vec![daemon(
            "default",
            vec![
                client(2, Some(true)),
                client(5, Some(true)),
                client(3, Some(true)),
            ],
        )]);
        assert_eq!(directory.followed, Some(key("default", 5)));
        // One reading of several daemons: the last socket path, whatever
        // the order they are given in and whatever the client ids.
        let directory = Directory::default().updated(vec![
            daemon("work", vec![client(1, Some(true))]),
            daemon("default", vec![client(8, Some(true))]),
        ]);
        assert_eq!(directory.followed, Some(key("work", 1)));
    }

    #[test]
    fn with_nothing_followed_a_reporting_client_is_followed_anyway() {
        // A client whose focus is unknown, as a TUI is until its terminal
        // reports a focus change, before one that reports no focus.
        let fresh = Directory::default().updated(vec![
            daemon("default", vec![client(1, None), silent(9)]),
            daemon("work", vec![client(4, Some(false))]),
        ]);
        assert_eq!(followed(&fresh), Some(("default", 1, false)));
        assert!(!fresh.followed_client().unwrap().has_focus());

        // Of equals, the last by socket path and then client id.
        for (daemons, expected) in [
            (
                vec![
                    daemon("default", vec![client(7, Some(false))]),
                    daemon("work", vec![client(2, Some(false)), client(1, Some(false))]),
                ],
                key("work", 2),
            ),
            (
                vec![daemon("default", vec![client(3, None), client(6, None)])],
                key("default", 6),
            ),
        ] {
            assert_eq!(
                Directory::default().updated(daemons).followed,
                Some(expected)
            );
        }

        // It then stays followed like any other: a client that attaches
        // with unknown focus does not replace it.
        let unfocused =
            Directory::default().updated(vec![daemon("default", vec![client(1, Some(false))])]);
        assert_eq!(followed(&unfocused), Some(("default", 1, false)));
        let joined = unfocused.updated(vec![daemon(
            "default",
            vec![client(1, Some(false)), client(2, None)],
        )]);
        assert_eq!(followed(&joined), Some(("default", 1, false)));
        // One that takes focus does.
        let taken = joined.updated(vec![daemon(
            "default",
            vec![client(1, Some(false)), client(2, Some(true))],
        )]);
        assert_eq!(followed(&taken), Some(("default", 2, true)));

        // A client that reports focus is picked first, although it took
        // focus before the client that was followed and has now detached.
        let both = Directory::default()
            .updated(vec![
                daemon("default", vec![client(1, Some(true)), client(2, None)]),
                daemon("work", vec![]),
            ])
            .updated(vec![
                daemon("default", vec![client(1, Some(true)), client(2, None)]),
                daemon("work", vec![client(1, Some(true))]),
            ]);
        assert_eq!(both.followed, Some(key("work", 1)));
        let left = both.updated(vec![
            daemon("default", vec![client(1, Some(true)), client(2, None)]),
            daemon("work", vec![]),
        ]);
        assert_eq!(left.followed, Some(key("default", 1)));
    }

    #[test]
    fn a_failed_watch_keeps_the_followed_client_without_supplying_it() {
        let start = Directory::default().updated(vec![
            daemon("default", vec![client(1, Some(true))]),
            daemon("work", vec![client(2, Some(false))]),
        ]);
        assert_eq!(followed(&start), Some(("default", 1, true)));

        // The watch of its daemon fails: the client is kept, but nothing is
        // known about it, so there is no client to show, jump or type into,
        // and no other client is picked in its place.
        let down = start.updated(vec![
            offline("default"),
            daemon("work", vec![client(2, Some(false))]),
        ]);
        assert_eq!(down.followed, Some(key("default", 1)));
        assert_eq!(followed(&down), None);
        assert_eq!(down.client_count(), 1);

        // The daemon lists it again, behind another app by now.
        let back = down.updated(vec![
            daemon("default", vec![client(1, Some(false))]),
            daemon("work", vec![client(2, Some(false))]),
        ]);
        assert_eq!(followed(&back), Some(("default", 1, false)));

        // While it is kept, a client that takes focus is followed instead.
        let taken = down.updated(vec![
            offline("default"),
            daemon("work", vec![client(2, Some(true))]),
        ]);
        assert_eq!(followed(&taken), Some(("work", 2, true)));
    }

    #[test]
    fn a_listing_after_a_failed_watch_is_compared_with_the_one_before_it() {
        // Both report focus; the client of `work` took it last.
        let both = vec![
            daemon("default", vec![client(1, Some(true))]),
            daemon("work", vec![client(2, Some(true))]),
        ];
        let start = Directory::default().updated(both.clone());
        assert_eq!(start.followed, Some(key("work", 2)));

        // The other daemon's watch fails and recovers. Its client reported
        // focus all along, so it did not take it.
        let down = start.updated(vec![offline("default"), both[1].clone()]);
        assert_eq!(followed(&down), Some(("work", 2, true)));
        let back = down.updated(both);
        assert_eq!(followed(&back), Some(("work", 2, true)));

        // A client that reports focus after the gap, and did not before it,
        // took it.
        let start = Directory::default().updated(vec![
            daemon("default", vec![client(1, Some(false))]),
            daemon("work", vec![client(2, Some(true))]),
        ]);
        let down = start.updated(vec![
            offline("default"),
            daemon("work", vec![client(2, Some(true))]),
        ]);
        let back = down.updated(vec![
            daemon("default", vec![client(1, Some(true))]),
            daemon("work", vec![client(2, Some(true))]),
        ]);
        assert_eq!(followed(&back), Some(("default", 1, true)));
    }

    #[test]
    fn the_followed_client_is_forgotten_when_a_listing_lacks_it_or_its_daemon_is_dropped() {
        let listed = vec![
            daemon("default", vec![client(1, Some(true))]),
            daemon("work", vec![client(2, Some(false))]),
        ];
        let start = Directory::default().updated(listed.clone());
        let down = start.updated(vec![offline("default"), listed[1].clone()]);
        assert_eq!(down.followed, Some(key("default", 1)));

        // The first listing after the failure does not contain it.
        let detached = down.updated(vec![daemon("default", vec![silent(9)]), listed[1].clone()]);
        assert_eq!(followed(&detached), Some(("work", 2, false)));

        // The daemon restarted: the same client id under another boot is
        // another client, which here took focus.
        let mut restarted = daemon("default", vec![client(1, Some(true))]);
        restarted.state = DaemonState::Following {
            boot_id: "boot-2".into(),
        };
        let next = down.updated(vec![restarted.clone(), listed[1].clone()]);
        assert_eq!(
            next.followed,
            Some(ClientKey {
                boot_id: "boot-2".into(),
                ..key("default", 1)
            })
        );
        // One that does not report focus is followed only for want of a
        // better: the other daemon's client is the last by socket path.
        restarted.clients = vec![client(1, Some(false))];
        let next = down.updated(vec![restarted, listed[1].clone()]);
        assert_eq!(next.followed, Some(key("work", 2)));

        // A discovery no longer lists the daemon: it is dropped, and the
        // followed client with it, whether its watch had failed or not.
        for before in [&start, &down] {
            let dropped = before.updated(vec![listed[1].clone()]);
            assert_eq!(followed(&dropped), Some(("work", 2, false)));
        }
        assert_eq!(start.updated(vec![]).followed, None);
        // Dropped and found again, its clients are new to the follower.
        let again = start.updated(vec![]).updated(listed);
        assert_eq!(followed(&again), Some(("default", 1, true)));
    }

    #[test]
    fn listings_replace_the_clients_in_id_order_and_report_real_changes() {
        let mut directory = Directory::default();
        let path = Path::new("/run/default/herdr.sock");
        let listed = |clients| {
            Observation::Listed(ClientList {
                boot_id: "boot-2".into(),
                clients,
            })
        };
        // A daemon that is not followed is not observed.
        assert!(!directory.observe(path, listed(vec![client(7, Some(true))])));
        assert_eq!(directory, Directory::default());

        directory.add("default", path);
        assert_eq!(directory.daemons, [offline("default")]);
        assert!(!directory.observe(path, Observation::Offline));
        assert!(directory.observe(path, listed(vec![client(7, Some(false)), client(3, None)])));
        let daemon = |directory: &Directory| directory.daemons[0].clone();
        assert_eq!(
            daemon(&directory).state,
            DaemonState::Following {
                boot_id: "boot-2".into()
            }
        );
        let ids = |directory: &Directory| {
            daemon(directory)
                .clients
                .iter()
                .map(|client| client.client_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&directory), [3, 7]);
        // The same clients in another order are no change.
        assert!(!directory.observe(path, listed(vec![client(3, None), client(7, Some(false))])));

        assert!(directory.observe(
            path,
            listed(vec![
                client(7, Some(true)),
                client(5, None),
                client(3, None)
            ])
        ));
        assert_eq!(ids(&directory), [3, 5, 7]);
        assert_eq!(daemon(&directory).clients[2], client(7, Some(true)));

        assert!(directory.observe(path, Observation::Offline));
        assert!(daemon(&directory).clients.is_empty());
        assert!(!directory.observe(path, Observation::Offline));

        // Daemons are kept in socket path order, and one added again keeps
        // its view.
        directory.add("another", Path::new("/run/another/herdr.sock"));
        directory.add("default", path);
        assert_eq!(
            directory
                .daemons
                .iter()
                .map(|daemon| daemon.session.as_str())
                .collect::<Vec<_>>(),
            ["another", "default"]
        );
        directory.remove(path);
        assert_eq!(directory.daemons, [offline("another")]);
    }

    #[test]
    fn reachable_agents_are_those_of_online_endpoints_with_a_boot() {
        let mut report = report(Some(true));
        report.endpoints.push(ClientReportEndpoint {
            endpoint_id: "ssh:build".into(),
            label: "Build".into(),
            status: ClientEndpointStatus::RECONNECTING.into(),
            boot_id: Some("boot-b".into()),
        });
        let mut local = jump_row("local");
        local.agent.pane_id = "w1:p1".into();
        let mut remote = jump_row("ssh:build");
        remote.agent.pane_id = "w1:p2".into();
        report.agents = vec![remote, local, jump_row("ssh:unlisted")];
        let directory = Directory::default().updated(vec![daemon(
            "work",
            vec![ClientInfo {
                client_id: 4,
                report: Some(report),
            }],
        )]);
        let focused = directory.followed_client().unwrap();
        assert_eq!(
            focused
                .reachable_agents()
                .map(|row| row.agent.pane_id.as_str())
                .collect::<Vec<_>>(),
            ["w1:p1"]
        );
        // The others are still in the report; a jump cannot show them now.
        assert_eq!(
            focused
                .report
                .agents
                .iter()
                .map(|row| focused.can_reach(row))
                .collect::<Vec<_>>(),
            [false, true, false]
        );
    }

    #[test]
    fn a_jump_names_the_boot_of_an_online_endpoint_only() {
        let mut report = report(Some(true));
        report.endpoints.push(ClientReportEndpoint {
            endpoint_id: "ssh:build".into(),
            label: "Build".into(),
            status: ClientEndpointStatus::RECONNECTING.into(),
            boot_id: Some("boot-b".into()),
        });
        report.endpoints.push(ClientReportEndpoint {
            endpoint_id: "ssh:new".into(),
            label: "New".into(),
            status: ClientEndpointStatus::ONLINE.into(),
            boot_id: None,
        });
        let row = jump_row;
        let directory = Directory::default().updated(vec![daemon(
            "work",
            vec![ClientInfo {
                client_id: 4,
                report: Some(report),
            }],
        )]);
        let focused = directory.followed_client().unwrap();
        let target = focused.jump_to(&row("local")).unwrap();
        assert_eq!(
            target.params(),
            ClientJumpParams {
                client_id: 4,
                endpoint_id: "local".into(),
                boot_id: "boot".into(),
                pane_id: "w1:p1".into(),
            }
        );
        assert_eq!(
            target.client,
            ClientKey {
                socket_path: "/run/work/herdr.sock".into(),
                boot_id: "boot-work".into(),
                client_id: 4,
            }
        );
        for unavailable in ["ssh:build", "ssh:new", "ssh:unlisted"] {
            assert!(
                focused.jump_to(&row(unavailable)).is_none(),
                "{unavailable}"
            );
        }
    }
}
