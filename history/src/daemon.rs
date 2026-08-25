//! Daemon wiring: single-instance locking, the control listener, and the
//! pane-focus subscription with reconnect and replay reconciliation.

use std::{
    fs,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use herdr_client::{
    Client, Environment, Error as ClientError, EventSubscription, Subscription,
    unix::{SocketCleanup, bind_private_socket, create_private_dir},
};

use crate::{
    SOCKET_TIMEOUT, START_TIMEOUT, SessionPaths, build_hash,
    control::handle_control,
    log_error, now_ms, remove_socket,
    state::{State, record_snapshot_events, replay_suffix},
};

const RECONNECT_INTERVAL: Duration = Duration::from_millis(250);
const EXEC_MISSING_GRACE: Duration = Duration::from_secs(5);

pub(crate) fn run_daemon() -> Result<()> {
    let environment = Environment::load()?;
    environment.require_plugin()?;
    let socket_path = environment
        .socket_path
        .context("HERDR_SOCKET_PATH is not set")?;
    let executable = std::env::current_exe().context("cannot resolve executable")?;
    let build = build_hash()?;
    let paths = SessionPaths::new(&socket_path);
    create_private_dir(&paths.runtime_dir)
        .with_context(|| format!("cannot prepare {}", paths.runtime_dir.display()))?;

    let lock = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&paths.lock_file)
        .with_context(|| format!("cannot open {}", paths.lock_file.display()))?;
    let lock_deadline = Instant::now() + START_TIMEOUT;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(fs::TryLockError::WouldBlock) => {
                if UnixStream::connect(&paths.control_socket).is_ok() {
                    return Ok(());
                }
                if Instant::now() >= lock_deadline {
                    bail!("history daemon lock is held without a live control socket");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(fs::TryLockError::Error(error)) => {
                return Err(error)
                    .with_context(|| format!("cannot lock {}", paths.lock_file.display()));
            }
        }
    }
    fs::set_permissions(&paths.lock_file, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot chmod {}", paths.lock_file.display()))?;

    remove_socket(&paths.control_socket)?;
    let listener = bind_private_socket(&paths.control_socket)
        .with_context(|| format!("cannot bind {}", paths.control_socket.display()))?;
    listener.set_nonblocking(true)?;
    let _socket_cleanup = SocketCleanup(&paths.control_socket);
    let mut state = State::fresh();
    let mut server_identity = None;
    let mut connection = None;
    let mut reconnect_at = Instant::now();
    let mut reconnect_logged = false;
    let mut executable_missing_since: Option<Instant> = None;

    loop {
        // Existence only, deliberately not identity: rebuilds recreate this
        // path (identity churns on every no-op build), while an uninstall
        // leaves it gone with no future client to retire this daemon. The
        // grace period rides out cargo's unlink-then-relink window.
        if fs::symlink_metadata(&executable).is_err() {
            let missing_since = *executable_missing_since.get_or_insert_with(Instant::now);
            if missing_since.elapsed() >= EXEC_MISSING_GRACE {
                log_error("history daemon retiring: executable was deleted");
                return Ok(());
            }
        } else {
            executable_missing_since = None;
        }

        if connection.is_none() && Instant::now() >= reconnect_at {
            match connect_focus_stream(&socket_path, &mut state, &mut server_identity) {
                Ok(connected) => {
                    if reconnect_logged {
                        log_error("history reconnected");
                    }
                    connection = Some(connected);
                    reconnect_logged = false;
                }
                Err(error) => {
                    if !reconnect_logged {
                        log_error(&format!("history reconnect failed: {error:#}"));
                        reconnect_logged = true;
                    }
                    reconnect_at = Instant::now() + RECONNECT_INTERVAL;
                }
            }
        }

        if connection
            .as_ref()
            .is_some_and(|connected| connected.subscription.has_buffered_event())
        {
            match connection.as_mut().unwrap().record_next_event(&mut state) {
                Ok(true) => {}
                Ok(false) => {
                    connection = None;
                    reconnect_at = Instant::now();
                }
                Err(error) => {
                    log_error(&format!("{error:#}"));
                    connection = None;
                    reconnect_at = Instant::now();
                }
            }
            continue;
        }

        match wait_ready(
            &listener,
            connection.as_ref().map(|connected| &connected.subscription),
        )? {
            Ready::Event => {
                let result = connection.as_mut().unwrap().record_next_event(&mut state);
                match result {
                    Ok(true) => {}
                    Ok(false) => {
                        connection = None;
                        reconnect_at = Instant::now();
                    }
                    Err(error) => {
                        log_error(&format!("{error:#}"));
                        connection = None;
                        reconnect_at = Instant::now();
                    }
                }
            }
            Ready::Timeout => {
                let result = connection
                    .as_mut()
                    .map(|connected| connected.finish_replay(&mut state));
                if let Some(Err(error)) = result {
                    log_error(&format!("{error:#}"));
                    connection = None;
                    reconnect_at = Instant::now();
                }
            }
            Ready::Control => match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .context("cannot configure history client")?;
                    if !handle_control(
                        stream,
                        connection.as_ref().and_then(|connected| {
                            (connected.replay == ReplayPhase::Live).then_some(&connected.client)
                        }),
                        &mut state,
                        &paths.control_socket,
                        &build,
                    ) {
                        return Ok(());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error).context("cannot accept history command"),
            },
        }
    }
}

struct FocusConnection {
    client: Client,
    subscription: Subscription,
    replay: ReplayPhase,
    baseline_pane: Option<String>,
    replayed_panes: Vec<String>,
}

#[derive(PartialEq, Eq)]
enum ReplayPhase {
    Retained,
    Snapshot,
    Live,
}

impl FocusConnection {
    fn record_next_event(&mut self, state: &mut State) -> Result<bool> {
        match self.subscription.next_event() {
            Ok(Some(event)) => {
                if event.event == "pane_focused"
                    && let Some(pane_id) = event
                        .data
                        .get("pane_id")
                        .and_then(serde_json::Value::as_str)
                {
                    if self.replay != ReplayPhase::Live {
                        self.replayed_panes.push(pane_id.into());
                    } else {
                        state.expire_echoes(now_ms());
                        state.record(pane_id.into());
                    }
                }
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(ClientError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(true)
            }
            Err(error) => Err(error).context("pane focus subscription ended"),
        }
    }

    fn finish_replay(&mut self, state: &mut State) -> Result<()> {
        state.expire_echoes(now_ms());
        match self.replay {
            ReplayPhase::Live => {}
            ReplayPhase::Retained => {
                let start = replay_suffix(&self.replayed_panes, self.baseline_pane.as_deref());
                for pane_id in self.replayed_panes.drain(start..) {
                    state.record(pane_id);
                }
                self.replayed_panes.clear();
                self.baseline_pane = self
                    .client
                    .snapshot()
                    .context("cannot reconcile focused pane")?
                    .focused_pane_id;
                self.replay = ReplayPhase::Snapshot;
            }
            ReplayPhase::Snapshot => {
                record_snapshot_events(
                    state,
                    self.baseline_pane.take(),
                    std::mem::take(&mut self.replayed_panes),
                );
                self.replay = ReplayPhase::Live;
            }
        }
        Ok(())
    }
}

fn connect_focus_stream(
    socket_path: &Path,
    state: &mut State,
    server_identity: &mut Option<String>,
) -> Result<FocusConnection> {
    let identity = file_identity(socket_path).context("cannot inspect Herdr socket")?;
    let client = Client::new(socket_path).with_timeout(SOCKET_TIMEOUT);
    let snapshot = client.snapshot().context("cannot snapshot focused pane")?;
    let subscription = client
        .subscribe(&[EventSubscription::new("pane.focused")])
        .context("cannot subscribe to pane focus events")?;
    if file_identity(socket_path).ok().as_ref() != Some(&identity) {
        bail!("Herdr server changed while history connected");
    }
    subscription
        .set_receive_timeout(SOCKET_TIMEOUT)
        .context("cannot bound focus event reads")?;
    if server_identity.as_ref() != Some(&identity) {
        *state = State::fresh();
        *server_identity = Some(identity);
    }
    state.expire_echoes(now_ms());
    if let Some(pane_id) = snapshot.focused_pane_id.clone() {
        state.record(pane_id);
    }
    Ok(FocusConnection {
        client,
        subscription,
        replay: ReplayPhase::Retained,
        baseline_pane: snapshot.focused_pane_id,
        replayed_panes: Vec::new(),
    })
}

enum Ready {
    Event,
    Control,
    Timeout,
}

fn wait_ready(listener: &UnixListener, subscription: Option<&Subscription>) -> Result<Ready> {
    let mut descriptors = vec![libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    if let Some(subscription) = subscription {
        descriptors.push(libc::pollfd {
            fd: subscription.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
    }
    // SAFETY: descriptors owns initialized pollfd values for this call.
    let result = unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            descriptors.len() as _,
            SOCKET_TIMEOUT.as_millis() as _,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("cannot wait for history activity");
    }
    if result == 0 {
        Ok(Ready::Timeout)
    } else if descriptors
        .get(1)
        .is_some_and(|descriptor| descriptor.revents != 0)
    {
        Ok(Ready::Event)
    } else {
        Ok(Ready::Control)
    }
}

fn file_identity(path: &Path) -> std::io::Result<String> {
    let metadata = fs::metadata(path)?;
    Ok(format!(
        "{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec()
    ))
}
