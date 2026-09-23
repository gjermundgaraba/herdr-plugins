// Space Meta: publishes composite, PR-aware workspace tokens for the spaces sidebar.
//
// One daemon per Herdr session. It subscribes to workspace events over the
// socket, watches each space's checkout directory with FSEvents, and runs a
// single `git status` per checkout directory per change. The only periodic
// work is the PR refresh.
mod fsevents;
mod git;
mod sidebar;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use herdr_client::{
    Client, Environment, Error, EventSubscription, SessionSnapshot, open_rotating_log,
    socket_scope_dir,
};
use serde_json::json;

use git::{Checkout, PrAnswer};
use sidebar::Tokens;

const SOURCE_ID: &str = "gjermundgaraba.herdr-space-meta";
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);
/// Coalesce bursts of Herdr events into one snapshot fetch.
const EVENT_SETTLE: Duration = Duration::from_millis(100);
/// FSEvents batches filesystem activity for this long before one callback.
const FSEVENTS_LATENCY_SECS: f64 = 0.75;
/// Every branch's PR answer is refreshed this often; a branch switch refreshes
/// immediately.
const PR_RECHECK: Duration = Duration::from_secs(5 * 60);
/// Wake-up interval when nothing is scheduled.
const IDLE_WAKE: Duration = Duration::from_secs(24 * 60 * 60);
const DAEMON_STOP_WAIT: Duration = Duration::from_secs(2);
const LOG_MAX_BYTES: u64 = 256 * 1024;

/// Everything that can change a space's number, label, or first pane. Herdr
/// emits nothing when a pane changes directory, so a non-worktree space's
/// checkout is re-sampled at these events only.
const SUBSCRIPTIONS: &[&str] = &[
    "workspace.created",
    "workspace.updated",
    "workspace.renamed",
    "workspace.closed",
    "workspace.moved",
    "workspace.reordered",
    "worktree.created",
    "worktree.opened",
    "worktree.removed",
    "tab.closed",
    "tab.moved",
    "pane.created",
    "pane.closed",
    "pane.moved",
];

fn main() {
    if let Err(error) = run() {
        eprintln!("space-meta: {error}");
        std::process::exit(1);
    }
}

struct Paths {
    socket: PathBuf,
    /// Flocked by the running daemon, which writes its pid into it.
    lock: PathBuf,
    log: PathBuf,
}

fn run() -> Result<(), String> {
    let environment = Environment::load().map_err(|error| error.to_string())?;
    let plugin = environment
        .require_plugin()
        .map_err(|error| error.to_string())?;
    let socket = environment
        .socket_path
        .clone()
        .ok_or("HERDR_SOCKET_PATH is not set")?;
    let run_dir = socket_scope_dir(&plugin.run_dir().join("sessions"), &socket);
    fs::create_dir_all(&run_dir).map_err(|error| format!("create run directory: {error}"))?;
    let paths = Paths {
        socket,
        lock: run_dir.join("daemon.lock"),
        log: plugin.logs_dir().join("space-meta.log"),
    };
    if std::env::args().nth(1).as_deref() == Some("--daemon") {
        return daemon(&paths);
    }
    if environment.action_id.as_deref() == Some("refresh") {
        stop_daemon(&paths)?;
    }
    start_daemon(&paths)
}

fn open_lock(path: &Path) -> Result<File, String> {
    File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("open {}: {error}", path.display()))
}

/// Whether the daemon lock is currently free (the lock is dropped again).
fn daemon_stopped(lock: &File) -> Result<bool, String> {
    match lock.try_lock() {
        Ok(()) => {
            lock.unlock().map_err(|error| format!("unlock: {error}"))?;
            Ok(true)
        }
        Err(fs::TryLockError::WouldBlock) => Ok(false),
        Err(fs::TryLockError::Error(error)) => Err(format!("check daemon lock: {error}")),
    }
}

/// The `refresh` action restarts the daemon so a rebuilt executable takes
/// over and every token is republished.
fn stop_daemon(paths: &Paths) -> Result<(), String> {
    let lock = open_lock(&paths.lock)?;
    if daemon_stopped(&lock)? {
        return Ok(());
    }
    let pid = fs::read_to_string(&paths.lock)
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
        .ok_or("daemon holds the lock but recorded no pid")?;
    // SAFETY: plain signal delivery; the pid was written under the lock that
    // is still held, so it names the live daemon.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(format!(
            "stop daemon {pid}: {}",
            std::io::Error::last_os_error()
        ));
    }
    let deadline = Instant::now() + DAEMON_STOP_WAIT;
    while !daemon_stopped(&lock)? {
        if Instant::now() >= deadline {
            return Err(format!("daemon {pid} did not stop"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn start_daemon(paths: &Paths) -> Result<(), String> {
    if !daemon_stopped(&open_lock(&paths.lock)?)? {
        return Ok(());
    }
    // The child takes the lock itself, so concurrent starters are harmless.
    Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("start daemon: {error}"))?;
    Ok(())
}

enum Msg {
    /// Any subscribed Herdr event; the snapshot is refetched, not the payload.
    Event,
    SubscriptionEnded,
    Changed(PathBuf),
    Resolved {
        cwd: PathBuf,
        /// Directories to watch plus the first scan; `None` when git failed.
        result: Option<(Vec<PathBuf>, Checkout)>,
    },
    Scanned {
        cwd: PathBuf,
        /// `None` when git failed.
        checkout: Option<Checkout>,
    },
    Pr {
        cwd: PathBuf,
        branch: String,
        answer: PrAnswer,
    },
}

/// What the daemon knows about one space's checkout directory. Any git or
/// watcher failure lands in `NotRepo`, which `reconcile` re-resolves at every
/// Herdr event; a real non-repository simply keeps failing cheaply.
enum Dir {
    /// First scan in flight; the row keeps whatever Herdr shows until it lands.
    Resolving,
    NotRepo,
    Tracked(Tracked),
}

struct Tracked {
    checkout: Checkout,
    scan: Scan,
    /// Lookup for `checkout.branch`; reset whenever the branch changes.
    pr: Pr,
    /// Held for its `Drop`; the stream reports through `Msg::Changed`.
    _watch: fsevents::Stream,
}

/// One scan in flight per directory; activity during a scan yields one more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    Idle,
    Running,
    RunningThenAgain,
}

impl Scan {
    /// Whether the caller should start a scan now.
    fn request(&mut self) -> bool {
        match *self {
            Self::Idle => {
                *self = Self::Running;
                true
            }
            Self::Running => {
                *self = Self::RunningThenAgain;
                false
            }
            Self::RunningThenAgain => false,
        }
    }

    /// Whether the caller should start the follow-up scan now.
    fn finished(&mut self) -> bool {
        let again = *self == Self::RunningThenAgain;
        *self = if again { Self::Running } else { Self::Idle };
        again
    }
}

/// PR knowledge for one branch. A recheck keeps the last number while pending
/// so the badge does not blink; a failed lookup keeps it too.
#[derive(Debug, Default, PartialEq, Eq)]
enum Pr {
    #[default]
    Unknown,
    Pending {
        keep: Option<String>,
    },
    Answered {
        number: Option<String>,
        at: Instant,
    },
}

impl Pr {
    fn number(&self) -> Option<&str> {
        match self {
            Self::Unknown => None,
            Self::Pending { keep: number } | Self::Answered { number, .. } => number.as_deref(),
        }
    }

    fn recheck_at(&self) -> Option<Instant> {
        match self {
            Self::Answered { at, .. } => Some(*at + PR_RECHECK),
            _ => None,
        }
    }

    /// Whether the caller should start a lookup now.
    fn start_if_due(&mut self, now: Instant) -> bool {
        let keep = match self {
            Self::Unknown => None,
            Self::Answered { number, at } if *at + PR_RECHECK <= now => number.take(),
            _ => return false,
        };
        *self = Self::Pending { keep };
        true
    }

    fn apply(&mut self, answer: PrAnswer) {
        let Self::Pending { keep } = self else {
            return;
        };
        let number = match answer {
            PrAnswer::Found(number) => Some(number),
            PrAnswer::NotFound => None,
            PrAnswer::Failed => keep.take(),
        };
        *self = Self::Answered {
            number,
            at: Instant::now(),
        };
    }
}

struct Daemon {
    client: Client,
    log: File,
    tx: Sender<Msg>,
    snapshot: SessionSnapshot,
    snapshot_due: Option<Instant>,
    dirs: HashMap<PathBuf, Dir>,
    published: HashMap<String, Tokens>,
}

fn daemon(paths: &Paths) -> Result<(), String> {
    let mut lock = open_lock(&paths.lock)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Ok(()),
        Err(fs::TryLockError::Error(error)) => return Err(format!("lock daemon: {error}")),
    }
    lock.set_len(0)
        .and_then(|()| lock.write_all(std::process::id().to_string().as_bytes()))
        .and_then(|()| lock.flush())
        .map_err(|error| format!("record pid: {error}"))?;
    let mut log = open_rotating_log(&paths.log, LOG_MAX_BYTES, 2)
        .map_err(|error| format!("open log: {error}"))?;
    let result = serve_session(paths, &log);
    if let Err(error) = &result {
        log_line(&mut log, error);
    }
    result
}

fn serve_session(paths: &Paths, log: &File) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    // Subscribe before the first snapshot so nothing can change unseen in between.
    spawn_subscription(&paths.socket, tx.clone())?;
    let client = Client::new(&paths.socket).with_timeout(SOCKET_TIMEOUT);
    let snapshot = client
        .snapshot()
        .map_err(|error| format!("snapshot session: {error}"))?;
    let mut daemon = Daemon {
        client,
        log: log
            .try_clone()
            .map_err(|error| format!("clone log handle: {error}"))?,
        tx,
        snapshot,
        snapshot_due: None,
        dirs: HashMap::new(),
        published: HashMap::new(),
    };
    daemon.reconcile();
    daemon.serve(rx);
    Ok(())
}

fn log_line(log: &mut File, message: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = writeln!(log, "{now} {message}");
}

fn spawn_subscription(socket: &Path, tx: Sender<Msg>) -> Result<(), String> {
    let subscriptions: Vec<_> = SUBSCRIPTIONS
        .iter()
        .map(|kind| EventSubscription::new(*kind))
        .collect();
    let mut subscription = Client::new(socket)
        .with_timeout(SOCKET_TIMEOUT)
        .subscribe(&subscriptions)
        .map_err(|error| format!("subscribe: {error}"))?;
    std::thread::spawn(move || {
        loop {
            match subscription.next_event() {
                Ok(Some(_)) => {
                    if tx.send(Msg::Event).is_err() {
                        return;
                    }
                }
                // A malformed frame is skipped; anything else ends the session.
                Err(Error::Json(_)) => {}
                Ok(None) | Err(_) => {
                    let _ = tx.send(Msg::SubscriptionEnded);
                    return;
                }
            }
        }
    });
    Ok(())
}

// Each subprocess job is its own short-lived thread; the per-directory states
// already keep at most one scan and one PR lookup in flight per directory.
fn spawn_resolve(tx: &Sender<Msg>, cwd: PathBuf) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = git::watch_roots(&cwd).and_then(|roots| Some((roots, git::scan(&cwd)?)));
        let _ = tx.send(Msg::Resolved { cwd, result });
    });
}

fn spawn_scan(tx: &Sender<Msg>, cwd: PathBuf) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        let checkout = git::scan(&cwd);
        let _ = tx.send(Msg::Scanned { cwd, checkout });
    });
}

fn spawn_pr(tx: &Sender<Msg>, cwd: PathBuf, branch: String) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        let answer = git::pr_number(&cwd, &branch);
        let _ = tx.send(Msg::Pr {
            cwd,
            branch,
            answer,
        });
    });
}

fn watch(tx: &Sender<Msg>, cwd: &Path, roots: &[PathBuf]) -> Result<fsevents::Stream, String> {
    let paths: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let tx = tx.clone();
    let cwd = cwd.to_path_buf();
    fsevents::Stream::new(&paths, FSEVENTS_LATENCY_SECS, move || {
        let _ = tx.send(Msg::Changed(cwd.clone()));
    })
}

impl Daemon {
    fn log(&mut self, message: &str) {
        log_line(&mut self.log, message);
    }

    fn serve(&mut self, rx: Receiver<Msg>) {
        loop {
            let now = Instant::now();
            let timeout = self.next_deadline().map_or(IDLE_WAKE, |deadline| {
                deadline.saturating_duration_since(now)
            });
            match rx.recv_timeout(timeout) {
                Ok(Msg::Event) => {
                    self.snapshot_due.get_or_insert(now + EVENT_SETTLE);
                }
                Ok(Msg::SubscriptionEnded) => return,
                Ok(Msg::Changed(cwd)) => {
                    if let Some(Dir::Tracked(tracked)) = self.dirs.get_mut(&cwd)
                        && tracked.scan.request()
                    {
                        spawn_scan(&self.tx, cwd);
                    }
                }
                Ok(Msg::Resolved { cwd, result }) => self.resolved(cwd, result),
                Ok(Msg::Scanned { cwd, checkout }) => self.scanned(cwd, checkout),
                Ok(Msg::Pr {
                    cwd,
                    branch,
                    answer,
                }) => {
                    // An answer for a branch no longer checked out is stale.
                    if let Some(Dir::Tracked(tracked)) = self.dirs.get_mut(&cwd)
                        && tracked.checkout.branch.as_deref() == Some(branch.as_str())
                    {
                        tracked.pr.apply(answer);
                    }
                }
                // Only the timeout; the daemon owns a sender, so no disconnect.
                Err(_) => {}
            }
            self.tick();
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let rechecks = self.dirs.values().filter_map(|dir| match dir {
            Dir::Tracked(tracked) => tracked.pr.recheck_at(),
            _ => None,
        });
        self.snapshot_due.into_iter().chain(rechecks).min()
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.snapshot_due.is_some_and(|due| due <= now) {
            self.snapshot_due = None;
            match self.client.snapshot() {
                Ok(snapshot) => {
                    self.snapshot = snapshot;
                    self.reconcile();
                }
                Err(error) => self.log(&format!("snapshot session: {error}")),
            }
        }
        for (cwd, dir) in &mut self.dirs {
            if let Dir::Tracked(tracked) = dir
                && let Some(branch) = tracked.checkout.branch.clone()
                && tracked.pr.start_if_due(now)
            {
                spawn_pr(&self.tx, cwd.clone(), branch);
            }
        }
        self.publish();
    }

    /// Align tracked directories and published-token memory with the snapshot.
    fn reconcile(&mut self) {
        let wanted: HashSet<PathBuf> = self
            .snapshot
            .workspaces
            .iter()
            .filter_map(|workspace| sidebar::workspace_cwd(&self.snapshot, workspace))
            .collect();
        self.dirs.retain(|cwd, _| wanted.contains(cwd));
        for cwd in wanted {
            match self.dirs.get(&cwd) {
                None => {
                    spawn_resolve(&self.tx, cwd.clone());
                    self.dirs.insert(cwd, Dir::Resolving);
                }
                Some(Dir::NotRepo) => spawn_resolve(&self.tx, cwd),
                Some(Dir::Resolving | Dir::Tracked(_)) => {}
            }
        }
        let ids: HashSet<&str> = self
            .snapshot
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_id.as_str())
            .collect();
        self.published.retain(|id, _| ids.contains(id.as_str()));
    }

    fn resolved(&mut self, cwd: PathBuf, result: Option<(Vec<PathBuf>, Checkout)>) {
        match self.dirs.get(&cwd) {
            Some(Dir::Resolving | Dir::NotRepo) => {}
            // Removed meanwhile, or a duplicate resolve already tracked it.
            None | Some(Dir::Tracked(_)) => return,
        }
        let dir = match result {
            None => Dir::NotRepo,
            Some((roots, checkout)) => match watch(&self.tx, &cwd, &roots) {
                Ok(watch) => Dir::Tracked(Tracked {
                    checkout,
                    scan: Scan::Idle,
                    pr: Pr::Unknown,
                    _watch: watch,
                }),
                Err(error) => {
                    self.log(&error);
                    Dir::NotRepo
                }
            },
        };
        self.dirs.insert(cwd, dir);
    }

    fn scanned(&mut self, cwd: PathBuf, checkout: Option<Checkout>) {
        let Some(Dir::Tracked(tracked)) = self.dirs.get_mut(&cwd) else {
            return;
        };
        let Some(checkout) = checkout else {
            // Gone or broken: forget it until the next Herdr event re-resolves it.
            self.dirs.insert(cwd, Dir::NotRepo);
            return;
        };
        if checkout.branch != tracked.checkout.branch {
            tracked.pr = Pr::Unknown;
        }
        tracked.checkout = checkout;
        if tracked.scan.finished() {
            spawn_scan(&self.tx, cwd);
        }
    }

    /// Report changed rows. Paused while a snapshot fetch is pending: after a
    /// report fails, the snapshot is the authority on which rows still exist
    /// and what to retry.
    fn publish(&mut self) {
        if self.snapshot_due.is_some() {
            return;
        }
        for (id, tokens) in render(&self.snapshot, &self.dirs) {
            if self.published.get(&id) == Some(&tokens) {
                continue;
            }
            let params = json!({
                "workspace_id": id,
                "source": SOURCE_ID,
                "tokens": {
                    "numbered_workspace": tokens.numbered_workspace,
                    "branch_line": tokens.branch_line,
                    "git_dirty": tokens.git_dirty,
                },
            });
            if let Err(error) = self.client.call_value("workspace.report_metadata", &params) {
                self.log(&format!("report metadata for {id}: {error}"));
                self.snapshot_due = Some(Instant::now() + EVENT_SETTLE);
                return;
            }
            self.published.insert(id, tokens);
        }
    }
}

/// Tokens for every workspace in sidebar order, except rows whose directory is
/// still resolving: those keep whatever Herdr already shows.
fn render(snapshot: &SessionSnapshot, dirs: &HashMap<PathBuf, Dir>) -> Vec<(String, Tokens)> {
    sidebar::display_order(snapshot)
        .into_iter()
        .enumerate()
        .filter_map(|(position, entry)| {
            let tracked = match sidebar::workspace_cwd(snapshot, entry.workspace)
                .and_then(|cwd| dirs.get(&cwd))
            {
                Some(Dir::Resolving) => return None,
                Some(Dir::Tracked(tracked)) => Some(tracked),
                Some(Dir::NotRepo) | None => None,
            };
            let tokens = sidebar::format_tokens(
                position + 1,
                &entry.workspace.label,
                tracked.and_then(|tracked| tracked.checkout.branch.as_deref()),
                tracked.and_then(|tracked| tracked.pr.number()),
                tracked.is_some_and(|tracked| tracked.checkout.dirty),
                entry.grouped_child,
            );
            Some((entry.workspace.workspace_id.clone(), tokens))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sidebar::tests::{snapshot, workspace};

    #[test]
    fn render_holds_resolving_rows_and_folds_in_tracked_facts() {
        let snapshot = snapshot(vec![
            workspace("ws", Some(("repo", false))),
            workspace("plain", None),
        ]);
        let cwd = PathBuf::from("/ws");
        let mut dirs = HashMap::new();
        dirs.insert(cwd.clone(), Dir::Resolving);
        let rows = render(&snapshot, &dirs);
        assert_eq!(rows.len(), 1, "resolving row is held");
        assert_eq!(rows[0].0, "plain");
        assert_eq!(rows[0].1.numbered_workspace, "2 plain");

        let temp = tempfile::tempdir().unwrap();
        let watch = fsevents::Stream::new(&[temp.path()], 1.0, || {}).unwrap();
        dirs.insert(
            cwd.clone(),
            Dir::Tracked(Tracked {
                checkout: Checkout {
                    branch: Some("feature".into()),
                    dirty: true,
                },
                scan: Scan::Idle,
                pr: Pr::Answered {
                    number: Some("123".into()),
                    at: Instant::now(),
                },
                _watch: watch,
            }),
        );
        let tokens = &render(&snapshot, &dirs)[0].1;
        assert_eq!(tokens.numbered_workspace, "1 ws");
        assert_eq!(tokens.branch_line.as_deref(), Some("⠀⠀feature #123"));
        assert!(tokens.git_dirty.is_some());

        dirs.insert(cwd, Dir::NotRepo);
        let tokens = &render(&snapshot, &dirs)[0].1;
        assert_eq!(tokens.numbered_workspace, "1 ws");
        assert_eq!(tokens.branch_line, None);
        assert_eq!(tokens.git_dirty, None);
    }

    #[test]
    fn pr_lookups_run_once_then_periodically_and_keep_the_badge_meanwhile() {
        let now = Instant::now();
        let mut pr = Pr::Unknown;
        assert!(pr.start_if_due(now), "unknown: ask");
        assert!(!pr.start_if_due(now), "pending: no duplicate");
        assert_eq!(pr.number(), None);
        assert_eq!(pr.recheck_at(), None, "pending: no timer");

        pr.apply(PrAnswer::Found("1".into()));
        assert_eq!(pr.number(), Some("1"));
        assert!(pr.recheck_at().is_some());
        assert!(!pr.start_if_due(now), "fresh answer");
        pr.apply(PrAnswer::NotFound);
        assert_eq!(
            pr.number(),
            Some("1"),
            "answers outside a lookup are ignored"
        );

        let later = Instant::now() + PR_RECHECK;
        assert!(pr.start_if_due(later), "recheck due");
        assert_eq!(pr.number(), Some("1"), "badge stays while rechecking");
        pr.apply(PrAnswer::Failed);
        assert_eq!(pr.number(), Some("1"), "offline: badge survives");
        assert!(pr.recheck_at().is_some(), "and is rechecked later");

        assert!(pr.start_if_due(Instant::now() + PR_RECHECK));
        pr.apply(PrAnswer::NotFound);
        assert_eq!(pr.number(), None, "PR closed");
    }

    #[test]
    fn one_scan_runs_at_a_time_and_a_burst_during_it_yields_one_more() {
        let mut scan = Scan::Idle;
        assert!(scan.request(), "idle: start now");
        assert!(!scan.request(), "running: queue one");
        assert!(!scan.request(), "still one");
        assert!(scan.finished(), "exactly one follow-up");
        assert!(!scan.finished());
        assert_eq!(scan, Scan::Idle);
    }
}
