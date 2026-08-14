use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use herdr_client::{
    Client, Environment, Error as ClientError, EventSubscription, PluginInvocation, Subscription,
    open_rotating_log,
};
use serde_json::json;

const DAEMON_FLAG: &str = "--daemon";
const ACTIVATE_ACTION: &str = "gjermundgaraba.herdr-history.activate";
const MAX_ENTRIES: usize = 100;
const ECHO_TTL_MS: u64 = 1_500;
const SOCKET_TIMEOUT: Duration = Duration::from_millis(500);
const START_TIMEOUT: Duration = Duration::from_secs(2);
const JUMP_BUDGET: Duration = Duration::from_secs(1);
const RECONNECT_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, PartialEq, Eq)]
struct Echo {
    pane_id: String,
    created_at_ms: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct State {
    entries: Vec<String>,
    cursor: usize,
    echoes: Vec<Echo>,
}

impl State {
    fn fresh() -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
            echoes: Vec::new(),
        }
    }

    fn expire_echoes(&mut self, now: u64) {
        self.echoes
            .retain(|echo| now.saturating_sub(echo.created_at_ms) < ECHO_TTL_MS);
    }

    fn record(&mut self, pane_id: String) {
        if let Some(index) = self.echoes.iter().position(|echo| echo.pane_id == pane_id) {
            self.echoes.drain(..=index);
            return;
        }
        if self.entries.get(self.cursor) == Some(&pane_id) {
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(pane_id);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.drain(..self.entries.len() - MAX_ENTRIES);
        }
        self.cursor = self.entries.len() - 1;
    }

    fn plan_jump(&self, step: isize) -> Option<(usize, String)> {
        let index = self.cursor.checked_add_signed(step)?;
        self.entries
            .get(index)
            .cloned()
            .map(|pane_id| (index, pane_id))
    }

    fn push_echo(&mut self, pane_id: String, now: u64) {
        self.echoes.push(Echo {
            pane_id,
            created_at_ms: now,
        });
    }

    fn focus_failed(&mut self, index: usize) {
        let target = self.entries.remove(index);
        if index <= self.cursor {
            self.cursor -= 1;
        }
        if let Some(index) = self.echoes.iter().rposition(|echo| echo.pane_id == target) {
            self.echoes.remove(index);
        }
    }

    fn cancel_echo(&mut self, pane_id: &str) {
        if let Some(index) = self.echoes.iter().rposition(|echo| echo.pane_id == pane_id) {
            self.echoes.remove(index);
        }
    }
}

struct SessionPaths {
    runtime_dir: PathBuf,
    lock_file: PathBuf,
    control_socket: PathBuf,
}

impl SessionPaths {
    fn new(socket_path: &Path, build: &str) -> Self {
        // SAFETY: geteuid has no preconditions.
        let runtime_dir = std::env::temp_dir().join(format!("hh-{}", unsafe { libc::geteuid() }));
        Self {
            lock_file: runtime_dir.join(format!("{}.lock", runtime_key(socket_path, ""))),
            control_socket: runtime_dir.join(format!("{}.sock", runtime_key(socket_path, build))),
            runtime_dir,
        }
    }
}

fn main() -> ExitCode {
    let daemon = std::env::args().nth(1).as_deref() == Some(DAEMON_FLAG);
    match if daemon {
        run_daemon()
    } else {
        run_invocation()
    } {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if daemon {
                log_error(&error);
            } else {
                eprintln!("herdr-history: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run_invocation() -> Result<(), String> {
    let environment = Environment::load().map_err(|error| error.to_string())?;
    environment
        .require_plugin()
        .map_err(|error| error.to_string())?;
    let socket_path = environment
        .socket_path
        .as_deref()
        .ok_or("HERDR_SOCKET_PATH is not set")?;
    let (_, build) = current_executable()?;
    let paths = SessionPaths::new(socket_path, &build);

    match environment.invocation() {
        Some(PluginInvocation::Startup) | Some(PluginInvocation::Action("activate")) => {
            activate_daemon(&paths.control_socket)
        }
        Some(PluginInvocation::Action("back")) => run_active_command(&paths.control_socket, "back"),
        Some(PluginInvocation::Action("forward")) => {
            run_active_command(&paths.control_socket, "forward")
        }
        invocation => Err(format!("unknown Herdr invocation: {invocation:?}")),
    }
}

fn current_executable() -> Result<(PathBuf, String), String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("cannot resolve executable: {error}"))?;
    let identity = file_identity(&executable)
        .map_err(|error| format!("cannot inspect {}: {error}", executable.display()))?;
    Ok((executable, identity))
}

fn activate_daemon(control_socket: &Path) -> Result<(), String> {
    let deadline = Instant::now() + START_TIMEOUT;
    let mut spawned = false;
    while Instant::now() < deadline {
        if let Ok(stream) = UnixStream::connect(control_socket) {
            return send_command(stream, "activate");
        }
        if !spawned {
            spawn_daemon()?;
            spawned = true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("history daemon did not start".into())
}

fn spawn_daemon() -> Result<(), String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("cannot resolve executable: {error}"))?;
    let mut command = Command::new(executable);
    command
        .arg(DAEMON_FLAG)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command
        .spawn()
        .map_err(|error| format!("cannot start history daemon: {error}"))?;
    Ok(())
}

fn run_active_command(control_socket: &Path, command: &str) -> Result<(), String> {
    let stream = UnixStream::connect(control_socket).map_err(|_| {
        format!("history is not active; run `herdr plugin action invoke {ACTIVATE_ACTION}` first")
    })?;
    send_command(stream, command)
}

fn send_command(mut stream: UnixStream, command: &str) -> Result<(), String> {
    stream
        .set_read_timeout(Some(START_TIMEOUT))
        .map_err(|error| error.to_string())?;
    writeln!(stream, "{command}").map_err(|error| format!("cannot send {command}: {error}"))?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|error| format!("cannot read history daemon response: {error}"))?;
    let response = response.trim();
    if response == "ok" {
        Ok(())
    } else if let Some(error) = response.strip_prefix("error ") {
        Err(error.into())
    } else {
        Err(format!("invalid history daemon response: {response}"))
    }
}

fn run_daemon() -> Result<(), String> {
    let environment = Environment::load().map_err(|error| error.to_string())?;
    environment
        .require_plugin()
        .map_err(|error| error.to_string())?;
    let socket_path = environment
        .socket_path
        .ok_or("HERDR_SOCKET_PATH is not set")?;
    let (executable, build) = current_executable()?;
    let paths = SessionPaths::new(&socket_path, &build);
    create_private_runtime_dir(&paths.runtime_dir)?;

    let lock = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&paths.lock_file)
        .map_err(|error| format!("cannot open {}: {error}", paths.lock_file.display()))?;
    let lock_deadline = Instant::now() + START_TIMEOUT;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(fs::TryLockError::WouldBlock) => {
                if UnixStream::connect(&paths.control_socket).is_ok() {
                    return Ok(());
                }
                if Instant::now() >= lock_deadline {
                    return Err("history daemon lock is held without a live control socket".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(fs::TryLockError::Error(error)) => {
                return Err(format!(
                    "cannot lock {}: {error}",
                    paths.lock_file.display()
                ));
            }
        }
    }
    fs::set_permissions(&paths.lock_file, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot chmod {}: {error}", paths.lock_file.display()))?;

    remove_socket(&paths.control_socket)?;
    let listener = bind_private_socket(&paths.control_socket)?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let _socket_cleanup = SocketCleanup(&paths.control_socket);
    let mut state = State::fresh();
    let mut server_identity = None;
    let mut connection = None;
    let mut reconnect_at = Instant::now();
    let mut reconnect_logged = false;

    loop {
        if !executable_is_current(&executable, &build) {
            return Ok(());
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
                        log_error(&format!("history reconnect failed: {error}"));
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
                    log_error(&error);
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
                        log_error(&error);
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
                    log_error(&error);
                    connection = None;
                    reconnect_at = Instant::now();
                }
            }
            Ready::Control => match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .map_err(|error| format!("cannot configure history client: {error}"))?;
                    handle_control(
                        stream,
                        connection.as_ref().and_then(|connected| {
                            (!connected.replaying).then_some(&connected.client)
                        }),
                        &mut state,
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(format!("cannot accept history command: {error}")),
            },
        }
    }
}

fn executable_is_current(path: &Path, identity: &str) -> bool {
    file_identity(path).is_ok_and(|current| current == identity)
}

struct FocusConnection {
    client: Client,
    subscription: Subscription,
    replaying: bool,
    baseline_pane: Option<String>,
    replayed_panes: Vec<String>,
}

impl FocusConnection {
    fn record_next_event(&mut self, state: &mut State) -> Result<bool, String> {
        match self.subscription.next_event() {
            Ok(Some(event)) => {
                if event.event == "pane_focused"
                    && let Some(pane_id) = event
                        .data
                        .get("pane_id")
                        .and_then(serde_json::Value::as_str)
                {
                    if self.replaying {
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
            Err(error) => Err(format!("pane focus subscription ended: {error}")),
        }
    }

    fn finish_replay(&mut self, state: &mut State) -> Result<(), String> {
        if !self.replaying {
            return Ok(());
        }
        let start = replay_suffix(&self.replayed_panes, self.baseline_pane.as_deref());
        state.expire_echoes(now_ms());
        for pane_id in self.replayed_panes.drain(start..) {
            state.record(pane_id);
        }
        if let Some(pane_id) = self
            .client
            .snapshot()
            .map_err(|error| format!("cannot reconcile focused pane: {error}"))?
            .focused_pane_id
        {
            state.record(pane_id);
        }
        self.replaying = false;
        Ok(())
    }
}

fn replay_suffix(events: &[String], baseline: Option<&str>) -> usize {
    baseline
        .and_then(|pane| events.iter().rposition(|event| event == pane))
        .map_or(events.len(), |index| index + 1)
}

fn connect_focus_stream(
    socket_path: &Path,
    state: &mut State,
    server_identity: &mut Option<String>,
) -> Result<FocusConnection, String> {
    let identity = file_identity(socket_path)
        .map_err(|error| format!("cannot inspect Herdr socket: {error}"))?;
    let client = Client::new(socket_path).with_timeout(SOCKET_TIMEOUT);
    let snapshot = client
        .snapshot()
        .map_err(|error| format!("cannot snapshot focused pane: {error}"))?;
    let subscription = client
        .subscribe(&[EventSubscription::new("pane.focused")])
        .map_err(|error| format!("cannot subscribe to pane focus events: {error}"))?;
    if file_identity(socket_path).ok().as_ref() != Some(&identity) {
        return Err("Herdr server changed while history connected".into());
    }
    subscription
        .set_receive_timeout(SOCKET_TIMEOUT)
        .map_err(|error| format!("cannot bound focus event reads: {error}"))?;
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
        replaying: true,
        baseline_pane: snapshot.focused_pane_id,
        replayed_panes: Vec::new(),
    })
}

enum Ready {
    Event,
    Control,
    Timeout,
}

fn wait_ready(
    listener: &UnixListener,
    subscription: Option<&herdr_client::Subscription>,
) -> Result<Ready, String> {
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
        return Err(format!(
            "cannot wait for history activity: {}",
            std::io::Error::last_os_error()
        ));
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

fn handle_control(mut stream: UnixStream, client: Option<&Client>, state: &mut State) {
    let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
    let mut command = String::new();
    let result = peer_is_current_user(&stream)
        .and_then(|()| {
            BufReader::new(&stream)
                .read_line(&mut command)
                .map_err(|error| format!("cannot read history command: {error}"))
        })
        .and_then(|_| match command.trim() {
            "activate" => Ok(()),
            "back" => jump(client.ok_or("history is reconnecting")?, state, -1),
            "forward" => jump(client.ok_or("history is reconnecting")?, state, 1),
            command => Err(format!("unknown history command: {command}")),
        });
    let _ = match result {
        Ok(()) => writeln!(stream, "ok"),
        Err(error) => writeln!(stream, "error {error}"),
    };
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

fn runtime_key(path: &Path, build: &str) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET;
    for byte in path
        .as_os_str()
        .as_bytes()
        .iter()
        .copied()
        .chain([0])
        .chain(build.bytes())
    {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{:024x}", hash & ((1_u128 << 96) - 1))
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn peer_is_current_user(stream: &UnixStream) -> Result<(), String> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: both output pointers are valid and stream owns a live descriptor.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(format!(
            "cannot inspect history client: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: geteuid has no preconditions.
    if uid == unsafe { libc::geteuid() } {
        Ok(())
    } else {
        Err("history client belongs to another user".into())
    }
}

#[cfg(target_os = "linux")]
fn peer_is_current_user(stream: &UnixStream) -> Result<(), String> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: credentials and length are valid writable values for getsockopt.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    } != 0
    {
        return Err(format!(
            "cannot inspect history client: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: geteuid has no preconditions.
    if credentials.uid == unsafe { libc::geteuid() } {
        Ok(())
    } else {
        Err("history client belongs to another user".into())
    }
}

fn jump(client: &Client, state: &mut State, step: isize) -> Result<(), String> {
    let deadline = Instant::now() + JUMP_BUDGET;
    while Instant::now() < deadline {
        let Some((index, target)) = state.plan_jump(step) else {
            let end = if step < 0 { "oldest" } else { "newest" };
            let _ = client.call_value(
                "notification.show",
                &json!({ "title": format!("history: at {end}") }),
            );
            return Ok(());
        };
        state.push_echo(target.clone(), now_ms());
        match client.focus_pane(&target) {
            Ok(_) => {
                state.cursor = index;
                return Ok(());
            }
            Err(ClientError::Api(error)) if error.code == "pane_not_found" => {
                state.focus_failed(index);
            }
            Err(ClientError::Api(error)) => {
                state.cancel_echo(&target);
                return Err(format!("cannot focus {target}: {error}"));
            }
            Err(error) => return Err(format!("cannot focus {target}: {error}")),
        }
    }
    Ok(())
}

fn create_private_runtime_dir(path: &Path) -> Result<(), String> {
    fs::create_dir(path)
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(error)
            }
        })
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    // SAFETY: geteuid has no preconditions.
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(format!(
            "{} is not a directory owned by the current user",
            path.display()
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot chmod {}: {error}", path.display()))
}

fn bind_private_socket(path: &Path) -> Result<UnixListener, String> {
    let previous_umask = unsafe { libc::umask(0o077) };
    let result = UnixListener::bind(path);
    unsafe {
        libc::umask(previous_umask);
    }
    result.map_err(|error| format!("cannot bind {}: {error}", path.display()))
}

fn remove_socket(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
    }
}

struct SocketCleanup<'a>(&'a Path);

impl Drop for SocketCleanup<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn log_error(message: &str) {
    let path = Environment::load()
        .ok()
        .and_then(|environment| environment.require_plugin().ok())
        .map(|plugin| plugin.logs_dir().join("history.log"));
    if let Some(Ok(mut file)) = path.map(|path| open_rotating_log(&path, 10 << 20, 3)) {
        let _ = writeln!(file, "{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_visits_and_truncates_forward_history() {
        let mut state = visited(&["A", "B", "B", "C"]);
        assert_eq!(state.entries, ["A", "B", "C"]);
        state.cursor = 0;
        state.record("D".into());
        assert_eq!(state.entries, ["A", "D"]);
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn ordered_echoes_preserve_rapid_double_back() {
        let mut state = visited(&["A", "B", "C"]);
        state.push_echo("B".into(), 0);
        state.cursor = 1;
        state.push_echo("A".into(), 0);
        state.cursor = 0;
        state.record("B".into());
        state.record("A".into());
        assert_eq!(state.entries, ["A", "B", "C"]);
        assert_eq!(state.cursor, 0);
        assert!(state.echoes.is_empty());
    }

    #[test]
    fn prunes_dead_panes_in_both_directions() {
        let mut back = visited(&["A", "B", "C", "D"]);
        back.push_echo("C".into(), 0);
        back.focus_failed(2);
        assert_eq!(back.entries, ["A", "B", "D"]);
        assert_eq!(back.plan_jump(-1), Some((1, "B".into())));

        let mut forward = visited(&["A", "B", "C"]);
        forward.cursor = 0;
        forward.focus_failed(1);
        assert_eq!(forward.entries, ["A", "C"]);
        assert_eq!(forward.plan_jump(1), Some((1, "C".into())));
    }

    #[test]
    fn stops_at_ends_and_caps_history() {
        let mut state = fresh();
        assert_eq!(state.plan_jump(-1), None);
        for index in 0..MAX_ENTRIES + 20 {
            state.record(format!("p{index}"));
        }
        assert_eq!(state.entries.len(), MAX_ENTRIES);
        assert_eq!(state.entries[0], "p20");
        assert_eq!(state.cursor, MAX_ENTRIES - 1);
        assert_eq!(state.plan_jump(1), None);
    }

    #[test]
    fn expires_and_cancels_echoes() {
        let mut state = fresh();
        state.push_echo("A".into(), 1_000);
        state.expire_echoes(1_000 + ECHO_TTL_MS);
        assert!(state.echoes.is_empty());

        state.push_echo("A".into(), 2_000);
        state.push_echo("B".into(), 2_000);
        state.cancel_echo("A");
        assert_eq!(state.echoes[0].pane_id, "B");
    }

    #[test]
    fn server_instances_use_distinct_runtime_paths() {
        let first = SessionPaths::new(Path::new("/tmp/herdr.sock"), "build");
        let same_server_path = SessionPaths::new(Path::new("/tmp/herdr.sock"), "build");
        let rebuilt = SessionPaths::new(Path::new("/tmp/herdr.sock"), "rebuilt");

        assert_eq!(first.lock_file, same_server_path.lock_file);
        assert_eq!(first.control_socket, same_server_path.control_socket);
        assert_ne!(first.control_socket, rebuilt.control_socket);
        assert!(first.control_socket.as_os_str().len() < 104);
    }

    #[test]
    fn notices_executable_replacement() {
        let path = std::env::temp_dir().join(format!("herdr-history-test-{}", std::process::id()));
        let replacement = path.with_extension("new");
        fs::write(&path, b"old").unwrap();
        let identity = file_identity(&path).unwrap();
        assert!(executable_is_current(&path, &identity));
        fs::write(&replacement, b"new").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert!(!executable_is_current(&path, &identity));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn retained_replay_keeps_only_events_after_the_snapshot_boundary() {
        let events = ["B", "C", "B", "D"].map(str::to_owned);
        assert_eq!(replay_suffix(&events, Some("B")), 3);
        assert_eq!(replay_suffix(&events, Some("A")), events.len());
    }

    fn visited(panes: &[&str]) -> State {
        let mut state = fresh();
        for pane in panes {
            state.record((*pane).into());
        }
        state
    }

    fn fresh() -> State {
        State::fresh()
    }
}
