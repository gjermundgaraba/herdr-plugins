//! Discovery of the local Herdr sessions: the default one and every named
//! session, each its own daemon and socket.
//!
//! `herdr session list --json` is the authority. Only running sessions in
//! the standard layout are kept, so a listed path can never point a
//! long-lived tool at a socket outside Herdr's own directory.

use std::{
    env, fs,
    io::{self, Read},
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;

/// How long `herdr session list` may run before it is killed. It probes each
/// session's socket, so a wedged daemon can stall it.
pub const SESSION_LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// One running local session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSession {
    pub name: String,
    pub socket_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionListError {
    #[error("cannot run {} session list: {source}", herdr.display())]
    Spawn {
        herdr: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} session list failed with {status}: {stderr}", herdr.display())]
    Failed {
        herdr: PathBuf,
        status: ExitStatus,
        stderr: String,
    },
    #[error("{} session list did not finish within {timeout:?} and was killed", herdr.display())]
    TimedOut { herdr: PathBuf, timeout: Duration },
    #[error("cannot wait for {} session list: {source}", herdr.display())]
    Wait {
        herdr: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Herdr returned an invalid session list: {0}")]
    Json(#[source] serde_json::Error),
    #[error("Herdr session list has no default session")]
    NoDefault,
    #[error("Herdr session list has multiple default sessions")]
    MultipleDefaults,
    #[error("Herdr returned an invalid default session layout")]
    InvalidDefaultLayout,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveHerdrError {
    #[error("PATH is not set")]
    NoPath,
    #[error("cannot find herdr in PATH")]
    NotInPath,
    #[error("resolve herdr from {source_name}: {}: {source}", path.display())]
    Canonicalize {
        source_name: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("herdr from {source_name} is not executable: {}", path.display())]
    NotExecutable {
        source_name: &'static str,
        path: PathBuf,
    },
}

/// The `herdr` executable: `HERDR_BIN_PATH` when set, else the first one in
/// `PATH`. Herdr sets `HERDR_BIN_PATH` for everything a plugin runs; a
/// LaunchAgent has neither it nor the user's `PATH` unless its plist says so.
pub fn resolve_herdr() -> Result<PathBuf, ResolveHerdrError> {
    if let Some(path) = env::var_os("HERDR_BIN_PATH").filter(|value| !value.is_empty()) {
        return executable(PathBuf::from(path), "HERDR_BIN_PATH");
    }
    let path = env::var_os("PATH").ok_or(ResolveHerdrError::NoPath)?;
    env::split_paths(&path)
        .map(|directory| directory.join("herdr"))
        .find(|candidate| is_executable(candidate))
        .ok_or(ResolveHerdrError::NotInPath)
        .and_then(|candidate| executable(candidate, "PATH"))
}

fn executable(path: PathBuf, source_name: &'static str) -> Result<PathBuf, ResolveHerdrError> {
    let path = fs::canonicalize(&path).map_err(|source| ResolveHerdrError::Canonicalize {
        source_name,
        path,
        source,
    })?;
    if !is_executable(&path) {
        return Err(ResolveHerdrError::NotExecutable { source_name, path });
    }
    Ok(path)
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    #[error(transparent)]
    Resolve(#[from] ResolveHerdrError),
    #[error(transparent)]
    List(#[from] SessionListError),
}

/// Every running local session, listed by the `herdr` executable that
/// [`resolve_herdr`] finds now, within [`SESSION_LIST_TIMEOUT`].
pub fn discover() -> Result<Vec<LocalSession>, DiscoverError> {
    Ok(list_sessions(&resolve_herdr()?, SESSION_LIST_TIMEOUT)?)
}

/// Every running local session, sorted by name. `herdr session list` is
/// killed when it runs longer than `timeout`.
pub fn list_sessions(
    herdr: &Path,
    timeout: Duration,
) -> Result<Vec<LocalSession>, SessionListError> {
    let mut child = Command::new(herdr)
        .args(["session", "list", "--json"])
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_TAB_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| SessionListError::Spawn {
            herdr: herdr.to_owned(),
            source,
        })?;
    let deadline = Instant::now() + timeout;
    // Drained on their own threads, so a full pipe cannot stall the child.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let timed_out = || SessionListError::TimedOut {
        herdr: herdr.to_owned(),
        timeout,
    };
    let status = wait_until(&mut child, deadline)
        .map_err(|source| SessionListError::Wait {
            herdr: herdr.to_owned(),
            source,
        })?
        .ok_or_else(timed_out)?;
    // A child the listing started may still hold a pipe open; the readers
    // are left behind rather than waited for past the deadline. The grace
    // covers output of a listing that exited right at it.
    let left = || {
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(100))
    };
    let stdout = stdout.recv_timeout(left()).map_err(|_| timed_out())?;
    if !status.success() {
        let stderr = stderr.recv_timeout(left()).unwrap_or_default();
        return Err(SessionListError::Failed {
            herdr: herdr.to_owned(),
            status,
            stderr: String::from_utf8_lossy(&stderr).trim().to_owned(),
        });
    }
    parse_sessions(&stdout)
}

/// Reads `pipe` to its end on a thread, or as far as it can be read.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    if let Some(mut pipe) = pipe {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            let _ = sender.send(bytes);
        });
    }
    receiver
}

/// The child's exit status, or `None` after it was killed at `deadline`.
fn wait_until(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
    const POLL: Duration = Duration::from_millis(5);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            // It may exit between the check and the kill; reap it either way.
            let _ = child.kill();
            child.wait()?;
            return Ok(None);
        }
        thread::sleep(left.min(POLL));
    }
}

#[derive(Deserialize)]
struct SessionList {
    sessions: Vec<ListedSession>,
}

#[derive(Deserialize)]
struct ListedSession {
    name: String,
    running: bool,
    default: bool,
    session_dir: PathBuf,
    socket_path: PathBuf,
}

/// Parses `herdr session list --json` output.
pub fn parse_sessions(bytes: &[u8]) -> Result<Vec<LocalSession>, SessionListError> {
    let listed: SessionList = serde_json::from_slice(bytes).map_err(SessionListError::Json)?;
    let mut defaults = listed.sessions.iter().filter(|session| session.default);
    let default = defaults.next().ok_or(SessionListError::NoDefault)?;
    if defaults.next().is_some() {
        return Err(SessionListError::MultipleDefaults);
    }
    if default.name != "default"
        || !default.session_dir.is_absolute()
        || default.socket_path != default.session_dir.join("herdr.sock")
    {
        return Err(SessionListError::InvalidDefaultLayout);
    }
    let root = default.session_dir.clone();

    let mut sessions: Vec<_> = listed
        .sessions
        .into_iter()
        .filter(|session| session.running)
        .filter_map(|session| {
            let expected_dir = if session.default {
                root.clone()
            } else if session.name != "default" && valid_name(&session.name) {
                root.join("sessions").join(&session.name)
            } else {
                return None;
            };
            (session.session_dir == expected_dir
                && session.socket_path == session.session_dir.join("herdr.sock"))
            .then_some(LocalSession {
                name: session.name,
                socket_path: session.socket_path,
            })
        })
        .collect();
    sessions.sort_by(|left, right| left.name.cmp(&right.name));
    sessions.dedup_by(|left, right| left.name == right.name);
    Ok(sessions)
}

fn valid_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_one_valid_default_session() {
        assert!(matches!(
            parse_sessions(br#"{"sessions":[]}"#),
            Err(SessionListError::NoDefault)
        ));
        assert!(matches!(
            parse_sessions(
                br#"{"sessions":[
                    {"name":"default","running":true,"default":true,"session_dir":"relative","socket_path":"relative/herdr.sock"}
                ]}"#
            ),
            Err(SessionListError::InvalidDefaultLayout)
        ));
        assert!(matches!(
            parse_sessions(
                br#"{"sessions":[
                    {"name":"default","running":true,"default":true,"session_dir":"/xdg/herdr","socket_path":"/xdg/herdr/herdr.sock"},
                    {"name":"other","running":true,"default":true,"session_dir":"/other","socket_path":"/other/herdr.sock"}
                ]}"#
            ),
            Err(SessionListError::MultipleDefaults)
        ));
        assert!(matches!(
            parse_sessions(b"not json"),
            Err(SessionListError::Json(_))
        ));
    }

    #[test]
    fn session_list_keeps_running_standard_sessions() {
        let sessions = parse_sessions(
            br#"{"sessions":[
                {"name":"work","running":true,"default":false,"session_dir":"/xdg/herdr/sessions/work","socket_path":"/xdg/herdr/sessions/work/herdr.sock"},
                {"name":"default","running":true,"default":true,"session_dir":"/xdg/herdr","socket_path":"/xdg/herdr/herdr.sock"},
                {"name":"stopped","running":false,"default":false,"session_dir":"/xdg/herdr/sessions/stopped","socket_path":"/xdg/herdr/sessions/stopped/herdr.sock"},
                {"name":"custom","running":true,"default":false,"session_dir":"/tmp/custom","socket_path":"/tmp/custom/herdr.sock"},
                {"name":"../escape","running":true,"default":false,"session_dir":"/xdg/herdr/escape","socket_path":"/xdg/herdr/escape/herdr.sock"},
                {"name":"wrong","running":true,"default":false,"session_dir":"/xdg/herdr/sessions/other","socket_path":"/xdg/herdr/sessions/other/herdr.sock"},
                {"name":"bad-socket","running":true,"default":false,"session_dir":"/xdg/herdr/sessions/bad-socket","socket_path":"/tmp/bad.sock"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            sessions,
            [
                LocalSession {
                    name: "default".into(),
                    socket_path: "/xdg/herdr/herdr.sock".into(),
                },
                LocalSession {
                    name: "work".into(),
                    socket_path: "/xdg/herdr/sessions/work/herdr.sock".into(),
                }
            ]
        );
    }

    #[test]
    fn a_failing_or_missing_herdr_is_a_typed_error() {
        assert!(matches!(
            list_sessions(Path::new("/definitely/missing/herdr"), SESSION_LIST_TIMEOUT),
            Err(SessionListError::Spawn { .. })
        ));
        assert!(matches!(
            list_sessions(Path::new("/usr/bin/false"), SESSION_LIST_TIMEOUT),
            Err(SessionListError::Failed { .. })
        ));
    }

    /// A stand-in `herdr` executable running `body`.
    fn fake_herdr(body: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("herdr");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        (directory, path)
    }

    #[test]
    fn a_listing_is_read_from_the_executable() {
        let (_directory, herdr) = fake_herdr(
            r#"echo '{"sessions":[{"name":"default","running":true,"default":true,"session_dir":"/xdg/herdr","socket_path":"/xdg/herdr/herdr.sock"}]}'"#,
        );
        let sessions = list_sessions(&herdr, SESSION_LIST_TIMEOUT).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].socket_path, Path::new("/xdg/herdr/herdr.sock"));
    }

    #[test]
    fn a_hung_listing_is_killed_at_its_timeout() {
        let (_directory, herdr) = fake_herdr("exec sleep 30");
        let started = Instant::now();
        let error = list_sessions(&herdr, Duration::from_millis(200)).unwrap_err();
        assert!(
            matches!(
                error,
                SessionListError::TimedOut { timeout, .. } if timeout == Duration::from_millis(200)
            ),
            "{error}"
        );
        // Killed and reaped, not waited out.
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
