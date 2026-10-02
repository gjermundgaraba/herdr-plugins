//! Space Priority: marks spaces as priority through the `space_priority`
//! workspace metadata token. In the fork's priority agent order, blocked and
//! done agents in those spaces sort ahead of every other agent; the token's
//! value doubles as a sidebar marker.
//!
//! Each space carries its own mark, and closing a space forgets it. Metadata
//! tokens do not survive a server restart or live handoff, so marks are saved
//! per session and republished by the startup hook.
mod state;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herdr_client::{Client, Environment, PluginInvocation, WorkspaceInfo, socket_scope_dir};
use serde_json::json;

use state::State;

const SOURCE_ID: &str = "gjermundgaraba.herdr-space-priority";
/// Workspace metadata key the fork's agent order reads.
const TOKEN_KEY: &str = "space_priority";
/// The token's value, shown wherever the token is placed in the sidebar.
const MARK: &str = "★";
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("space-priority: {error:#}");
            if let Ok(client) = Client::from_env() {
                let _ = client.with_timeout(SOCKET_TIMEOUT).call_value(
                    "notification.show",
                    &json!({"title": "Space priority failed", "body": format!("{error:#}")}),
                );
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let environment = Environment::load()?;
    let state_path = state_path(&environment)?;
    let client = Client::from_env()?.with_timeout(SOCKET_TIMEOUT);
    match environment.invocation() {
        Some(PluginInvocation::Startup) => sync(&client, &state_path, None),
        Some(PluginInvocation::Event {
            name: "workspace.closed",
            event,
        }) => {
            let workspace_id = event.data["workspace_id"]
                .as_str()
                .context("workspace.closed event without workspace_id")?;
            sync(&client, &state_path, Some(workspace_id))
        }
        Some(PluginInvocation::Action(_)) => toggle(&client, &state_path),
        _ => bail!("run this through the plugin's action, startup hook, or event hook"),
    }
}

/// Marks are scoped to the session socket: workspace ids are per session.
fn state_path(environment: &Environment) -> Result<PathBuf> {
    let plugin = environment.require_plugin()?;
    let socket = environment
        .socket_path
        .as_deref()
        .context("HERDR_SOCKET_PATH is not set")?;
    Ok(socket_scope_dir(&plugin.data_dir(), socket).join("priority.json"))
}

/// Republishes the saved marks against the current spaces, leaving out a
/// space that is closing but may still be listed.
fn sync(client: &Client, state_path: &Path, closed: Option<&str>) -> Result<()> {
    let saved = State::load(state_path).context("read saved priority spaces")?;
    let mut workspaces = client
        .snapshot()
        .context("read session snapshot")?
        .workspaces;
    workspaces.retain(|workspace| Some(workspace.workspace_id.as_str()) != closed);
    publish(client, state_path, &saved, &saved.workspaces, &workspaces)
}

/// Flips the mark of the focused space.
fn toggle(client: &Client, state_path: &Path) -> Result<()> {
    let snapshot = client.snapshot().context("read session snapshot")?;
    let workspace_id = std::env::var("HERDR_WORKSPACE_ID")
        .ok()
        .filter(|id| !id.is_empty())
        .or(snapshot.focused_workspace_id)
        .context("no focused space")?;
    let workspaces = snapshot.workspaces;
    let target = workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == workspace_id)
        .context("the space is gone")?;
    let saved = State::load(state_path).context("read saved priority spaces")?;
    let mut marks = prune(&saved.workspaces, &workspaces);
    let marked = !marks.remove(&target.workspace_id);
    if marked {
        marks.insert(target.workspace_id.clone());
    }
    publish(client, state_path, &saved, &marks, &workspaces)?;
    let body = if marked {
        format!("{} is a priority space", target.label)
    } else {
        format!("{} is no longer a priority space", target.label)
    };
    client
        .call_value(
            "notification.show",
            &json!({"title": "Space priority", "body": body}),
        )
        .context("show the new priority")?;
    Ok(())
}

/// Saves `marks` without spaces that are gone and reports every token that
/// differs from them.
fn publish(
    client: &Client,
    state_path: &Path,
    saved: &State,
    marks: &BTreeSet<String>,
    workspaces: &[WorkspaceInfo],
) -> Result<()> {
    let marks = store(state_path, saved, marks, workspaces)?;
    for (workspace_id, mark) in changes(workspaces, &marks) {
        client
            .call_value(
                "workspace.report_metadata",
                &json!({
                    "workspace_id": workspace_id,
                    "source": SOURCE_ID,
                    "tokens": {TOKEN_KEY: mark},
                }),
            )
            .with_context(|| format!("publish the priority of {workspace_id}"))?;
    }
    Ok(())
}

/// Writes `marks` without spaces that are gone when they differ from the
/// `saved` state, and returns them.
fn store(
    state_path: &Path,
    saved: &State,
    marks: &BTreeSet<String>,
    workspaces: &[WorkspaceInfo],
) -> Result<BTreeSet<String>> {
    let state = State {
        workspaces: prune(marks, workspaces),
    };
    if state != *saved {
        state.save(state_path).context("save priority spaces")?;
    }
    Ok(state.workspaces)
}

/// Saved marks without spaces that are gone.
fn prune(saved: &BTreeSet<String>, workspaces: &[WorkspaceInfo]) -> BTreeSet<String> {
    saved
        .iter()
        .filter(|id| {
            workspaces
                .iter()
                .any(|workspace| &workspace.workspace_id == *id)
        })
        .cloned()
        .collect()
}

/// Token updates that give each space its saved mark.
fn changes<'a>(
    workspaces: &'a [WorkspaceInfo],
    marked: &BTreeSet<String>,
) -> Vec<(&'a str, Option<&'static str>)> {
    workspaces
        .iter()
        .filter_map(|workspace| {
            let want = marked.contains(&workspace.workspace_id).then_some(MARK);
            let have = workspace.tokens.get(TOKEN_KEY).map(String::as_str);
            (have != want).then_some((workspace.workspace_id.as_str(), want))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn space(id: &str, marked: bool) -> WorkspaceInfo {
        serde_json::from_value(json!({
            "workspace_id": id, "number": 1, "label": id, "focused": false,
            "pane_count": 1, "tab_count": 1, "active_tab_id": format!("{id}:t1"),
            "agent_status": "idle",
            "tokens": if marked { json!({TOKEN_KEY: MARK}) } else { json!({}) },
        }))
        .unwrap()
    }

    fn ids(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn gone_spaces_drop_from_the_marks() {
        let spaces = [space("kept", false), space("other", false)];
        assert_eq!(prune(&ids(&["kept", "closed"]), &spaces), ids(&["kept"]));
    }

    #[test]
    fn a_new_mark_is_saved_beside_the_earlier_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("priority.json");
        let spaces = [space("first", true), space("second", false)];
        let saved = State {
            workspaces: ids(&["first"]),
        };
        saved.save(&path).unwrap();
        let both = ids(&["first", "second"]);
        assert_eq!(store(&path, &saved, &both, &spaces).unwrap(), both);
        let saved = State::load(&path).unwrap();
        assert_eq!(saved.workspaces, both);
        assert!(store(&path, &saved, &ids(&[]), &spaces).unwrap().is_empty());
        assert!(State::load(&path).unwrap().workspaces.is_empty());
    }

    #[test]
    fn each_space_gets_its_own_mark() {
        let spaces = [
            space("kept", true),
            space("new", false),
            space("cleared", true),
            space("other", false),
        ];
        assert_eq!(
            changes(&spaces, &ids(&["kept", "new"])),
            [("new", Some(MARK)), ("cleared", None)]
        );
    }
}
