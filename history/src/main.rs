use std::{
    fs::{self, File, OpenOptions, TryLockError},
    path::Path,
    process::ExitCode,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use herdr_client::{Client, Environment, Error as ClientError};
use serde::{Deserialize, Serialize};
use serde_json::json;

const MAX_ENTRIES: usize = 100;
const ECHO_TTL_MS: u64 = 1_500;
const LOCK_RETRY: Duration = Duration::from_millis(10);
const LOCK_BUDGET: Duration = Duration::from_secs(3);
const SOCKET_TIMEOUT: Duration = Duration::from_millis(500);
const JUMP_BUDGET: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Echo {
    pane_id: String,
    created_at_ms: u64,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    server_generation: String,
    entries: Vec<String>,
    cursor: usize,
    echoes: Vec<Echo>,
}

impl State {
    fn fresh(server_generation: String) -> Self {
        Self {
            server_generation,
            entries: Vec::new(),
            cursor: 0,
            echoes: Vec::new(),
        }
    }

    fn load(raw: &[u8], server_generation: String) -> Self {
        serde_json::from_slice(raw)
            .ok()
            .filter(|state: &Self| {
                state.server_generation == server_generation
                    && ((state.entries.is_empty() && state.cursor == 0)
                        || state.cursor < state.entries.len())
            })
            .unwrap_or_else(|| Self::fresh(server_generation))
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
}

enum Mode {
    Record,
    Jump(isize),
}

fn acquire_lock(path: &Path, deadline: Instant) -> Result<Option<File>, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => thread::sleep(LOCK_RETRY),
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => {
                return Err(format!("cannot lock {}: {error}", path.display()));
            }
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let Environment {
        socket_path: Some(socket_path),
        plugin_state_dir: Some(state_dir),
        action_id,
        event_name,
        event,
        ..
    } = Environment::load().map_err(|error| error.to_string())?
    else {
        return Err("herdr-history must run under Herdr (missing HERDR_* env)".into());
    };
    let mode = match (event_name.as_deref(), action_id.as_deref()) {
        (Some("pane.focused"), _) => Mode::Record,
        (_, Some("back")) => Mode::Jump(-1),
        (_, Some("forward")) => Mode::Jump(1),
        invocation => return Err(format!("unknown Herdr invocation: {invocation:?}")),
    };
    let event_pane_id =
        event.and_then(|event| event.data.get("pane_id")?.as_str().map(ToOwned::to_owned));
    let session_dir = session_state_dir(&state_dir, &socket_path);
    fs::create_dir_all(&session_dir)
        .map_err(|error| format!("cannot create {}: {error}", session_dir.display()))?;
    let state_file = session_dir.join("history.json");
    let Some(_lock) = acquire_lock(&session_dir.join("lock"), Instant::now() + LOCK_BUDGET)? else {
        eprintln!("lock acquire timed out; dropping invocation");
        return Ok(());
    };

    let mut state = read_state(&state_file, server_generation(&socket_path)?)?;
    state.expire_echoes(now_ms());

    match mode {
        Mode::Record => {
            if let Some(pane_id) = event_pane_id {
                state.record(pane_id);
                save(&state_file, &state)?;
            }
        }
        Mode::Jump(step) => jump(&state_file, &socket_path, state, step)?,
    }
    Ok(())
}

fn jump(
    state_file: &Path,
    socket_path: &Path,
    mut state: State,
    step: isize,
) -> Result<(), String> {
    let deadline = Instant::now() + JUMP_BUDGET;
    let client = Client::new(socket_path).with_timeout(SOCKET_TIMEOUT);
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
        save(state_file, &state)?;
        match client.call_value("pane.focus", &json!({ "pane_id": target })) {
            Ok(_) => {
                state.cursor = index;
                save(state_file, &state)?;
                return Ok(());
            }
            Err(ClientError::Api(error)) if error.code == "pane_not_found" => {
                state.focus_failed(index);
                save(state_file, &state)?;
            }
            Err(error) => return Err(format!("cannot focus {target}: {error}")),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn session_state_dir(state_dir: &Path, socket_path: &Path) -> std::path::PathBuf {
    use std::{fmt::Write as _, os::unix::ffi::OsStrExt};

    let bytes = socket_path.as_os_str().as_bytes();
    let mut key = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut key, "{byte:02x}").unwrap();
    }
    state_dir.join("sessions").join(key)
}

#[cfg(unix)]
fn server_generation(socket_path: &Path) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;

    fs::metadata(socket_path)
        .map(|metadata| {
            format!(
                "{}:{}:{}:{}",
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec()
            )
        })
        .map_err(|error| format!("cannot inspect {}: {error}", socket_path.display()))
}

fn read_state(path: &Path, server_generation: String) -> Result<State, String> {
    match fs::read(path) {
        Ok(raw) => Ok(State::load(&raw, server_generation)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(State::fresh(server_generation))
        }
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn save(path: &Path, state: &State) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(state).map_err(|error| error.to_string())?;
    fs::write(&tmp, bytes)
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(|error| format!("cannot save {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENERATION: &str = "dev:ino:ctime:ctime_nsec";

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
    fn consumes_echoes_without_changing_history() {
        let mut state = visited(&["A", "B", "C"]);
        state.push_echo("B".into(), 0);
        state.push_echo("A".into(), 0);
        state.record("A".into());
        assert_eq!(state.entries, ["A", "B", "C"]);
        assert!(state.echoes.is_empty());
    }

    #[test]
    fn rapid_double_back_keeps_forward_history() {
        let mut state = visited(&["A", "B", "C"]);
        state.push_echo("B".into(), 0);
        state.cursor = 1;
        state.push_echo("A".into(), 0);
        state.cursor = 0;
        state.record("B".into());
        state.record("A".into());
        assert_eq!(state.entries, ["A", "B", "C"]);
        assert_eq!(state.cursor, 0);
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
    fn expires_echoes_and_rejects_invalid_state() {
        let mut state = fresh();
        state.push_echo("A".into(), 1_000);
        state.expire_echoes(1_000 + ECHO_TTL_MS);
        assert!(state.echoes.is_empty());

        let raw = serde_json::to_vec(&visited(&["A", "B"])).unwrap();
        assert_eq!(State::load(&raw, GENERATION.into()).cursor, 1);
        assert_eq!(
            State::load(&raw, "other".into()),
            State::fresh("other".into())
        );
        assert_eq!(State::load(b"not json", GENERATION.into()), fresh());
    }

    #[test]
    fn locks_are_isolated_by_session_and_released_on_drop() {
        let root = temp_path("locks");
        let first = session_state_dir(&root, Path::new("/tmp/first.sock"));
        let second = session_state_dir(&root, Path::new("/tmp/second.sock"));
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();

        let path = first.join("lock");
        let owner = acquire_lock(&path, Instant::now()).unwrap().unwrap();
        assert!(acquire_lock(&path, Instant::now()).unwrap().is_none());
        let other = acquire_lock(&second.join("lock"), Instant::now())
            .unwrap()
            .unwrap();
        drop(owner);
        let successor = acquire_lock(&path, Instant::now()).unwrap().unwrap();
        drop(other);
        drop(successor);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transport_failure_preserves_history_and_echo() {
        let state_file = temp_path("focus-state");
        let socket_path = temp_path("missing-socket");
        assert!(jump(&state_file, &socket_path, visited(&["A", "B"]), -1).is_err());

        let state: State = serde_json::from_slice(&fs::read(&state_file).unwrap()).unwrap();
        assert_eq!(state.entries, ["A", "B"]);
        assert_eq!(state.cursor, 1);
        assert_eq!(state.echoes[0].pane_id, "A");
        fs::remove_file(state_file).unwrap();
    }

    #[test]
    fn state_read_defaults_only_when_missing() {
        let path = temp_path("state");
        assert_eq!(read_state(&path, GENERATION.into()).unwrap(), fresh());
        fs::create_dir(&path).unwrap();
        assert!(read_state(&path, GENERATION.into()).is_err());
        fs::remove_dir(path).unwrap();
    }

    fn visited(panes: &[&str]) -> State {
        let mut state = fresh();
        for pane in panes {
            state.record((*pane).into());
        }
        state
    }

    fn fresh() -> State {
        State::fresh(GENERATION.into())
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-history-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }
}
