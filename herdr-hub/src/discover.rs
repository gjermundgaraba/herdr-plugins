use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
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
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set")?;
    parse_sessions(&output.stdout, &home.join(".config/herdr"))
}

/// Maps only Herdr's default and named-session socket layouts.
pub(crate) fn session_name(socket_path: &Path) -> Option<String> {
    let home = env::var_os("HOME").filter(|value| !value.is_empty())?;
    session_name_under(&PathBuf::from(home).join(".config/herdr"), socket_path)
}

fn session_name_under(root: &Path, socket_path: &Path) -> Option<String> {
    if socket_path == root.join("herdr.sock") {
        return Some("default".into());
    }
    if socket_path.file_name()? != "herdr.sock" {
        return None;
    }
    let parent = socket_path.parent()?;
    let name = parent.file_name()?.to_str()?;
    if name.is_empty() || parent.parent()? != root.join("sessions") {
        return None;
    }
    Some(name.into())
}

#[derive(Deserialize)]
struct SessionList {
    sessions: Vec<ListedSession>,
}

#[derive(Deserialize)]
struct ListedSession {
    name: String,
    running: bool,
    socket_path: PathBuf,
}

fn parse_sessions(bytes: &[u8], root: &Path) -> Result<Vec<LocalSession>> {
    let listed: SessionList =
        serde_json::from_slice(bytes).context("Herdr returned an invalid session list")?;
    let mut sessions: Vec<_> = listed
        .sessions
        .into_iter()
        .filter(|session| session.running)
        .filter_map(|session| {
            (session_name_under(root, &session.socket_path).as_deref() == Some(&session.name))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_session_socket_layouts() {
        let root = Path::new("/Users/test/.config/herdr");
        assert_eq!(
            session_name_under(root, Path::new("/Users/test/.config/herdr/herdr.sock")),
            Some("default".into())
        );
        assert_eq!(
            session_name_under(
                root,
                Path::new("/Users/test/.config/herdr/sessions/work/herdr.sock")
            ),
            Some("work".into())
        );
        assert_eq!(
            session_name_under(root, Path::new("/tmp/custom.sock")),
            None
        );
        assert_eq!(
            session_name_under(root, Path::new("/tmp/herdr/herdr.sock")),
            None
        );
        assert_eq!(
            session_name_under(root, Path::new("/Users/test/.config/other/work/herdr.sock")),
            None
        );
    }

    #[test]
    fn session_list_keeps_running_standard_sessions() {
        let sessions = parse_sessions(
            br#"{"sessions":[
                {"name":"work","running":true,"socket_path":"/Users/test/.config/herdr/sessions/work/herdr.sock"},
                {"name":"default","running":true,"socket_path":"/Users/test/.config/herdr/herdr.sock"},
                {"name":"stopped","running":false,"socket_path":"/Users/test/.config/herdr/sessions/stopped/herdr.sock"},
                {"name":"custom","running":true,"socket_path":"/tmp/custom.sock"},
                {"name":"suffix","running":true,"socket_path":"/tmp/herdr/herdr.sock"},
                {"name":"wrong","running":true,"socket_path":"/Users/test/.config/herdr/sessions/other/herdr.sock"}
            ]}"#,
            Path::new("/Users/test/.config/herdr"),
        )
        .unwrap();
        assert_eq!(
            sessions,
            [
                LocalSession {
                    name: "default".into(),
                    socket_path: "/Users/test/.config/herdr/herdr.sock".into(),
                },
                LocalSession {
                    name: "work".into(),
                    socket_path: "/Users/test/.config/herdr/sessions/work/herdr.sock".into(),
                }
            ]
        );
    }
}
