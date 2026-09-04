use std::{
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalSession {
    pub name: String,
    pub socket_path: PathBuf,
}

impl LocalSession {
    pub(crate) fn key(&self) -> String {
        format!("local/{}", self.name)
    }
}

pub(crate) fn list_sessions(herdr: &Path) -> Result<Vec<LocalSession>> {
    let output = Command::new(herdr)
        .args(["session", "list", "--json"])
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_TAB_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .output()
        .with_context(|| format!("cannot run {} session list", herdr.display()))?;
    if !output.status.success() {
        bail!(
            "{} session list failed with {}: {}",
            herdr.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_sessions(&output.stdout)
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

fn parse_sessions(bytes: &[u8]) -> Result<Vec<LocalSession>> {
    let listed: SessionList =
        serde_json::from_slice(bytes).context("Herdr returned an invalid session list")?;
    let mut defaults = listed.sessions.iter().filter(|session| session.default);
    let default = defaults
        .next()
        .context("Herdr session list has no default session")?;
    ensure!(
        defaults.next().is_none(),
        "Herdr session list has multiple default sessions"
    );
    ensure!(
        default.name == "default"
            && default.session_dir.is_absolute()
            && default.socket_path == default.session_dir.join("herdr.sock"),
        "Herdr returned an invalid default session layout"
    );
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
        assert!(parse_sessions(br#"{"sessions":[]}"#).is_err());
        assert!(
            parse_sessions(
                br#"{"sessions":[
                    {"name":"default","running":true,"default":true,"session_dir":"relative","socket_path":"relative/herdr.sock"}
                ]}"#
            )
            .is_err()
        );
        assert!(
            parse_sessions(
                br#"{"sessions":[
                    {"name":"default","running":true,"default":true,"session_dir":"/xdg/herdr","socket_path":"/xdg/herdr/herdr.sock"},
                    {"name":"other","running":true,"default":true,"session_dir":"/other","socket_path":"/other/herdr.sock"}
                ]}"#
            )
            .is_err()
        );
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
}
