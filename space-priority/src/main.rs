//! Space Priority: marks spaces as priority through the `space_priority`
//! workspace metadata token. In the fork's priority agent order, blocked and
//! done agents in those spaces sort ahead of every other agent; the token's
//! value doubles as a sidebar marker.
//!
//! A worktree family shares its checkout's mark, so every member carries the
//! token, and creating or closing a space re-syncs it. Metadata tokens do not
//! survive a server restart or live handoff, so marks are saved per session
//! and republished by the startup hook.
mod family;
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
        Some(
            PluginInvocation::Startup
            | PluginInvocation::Event {
                name: "workspace.created",
                ..
            },
        ) => sync(&client, &state_path, None),
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
    let state = State::load(state_path).context("read saved priority spaces")?;
    let mut workspaces = client
        .snapshot()
        .context("read session snapshot")?
        .workspaces;
    workspaces.retain(|workspace| Some(workspace.workspace_id.as_str()) != closed);
    publish(client, state_path, state, &workspaces)
}

/// Flips the mark of the focused space's family.
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
    let anchor = family::anchor(&workspaces, target);
    let mut state = State::load(state_path).context("read saved priority spaces")?;
    state.workspaces = resolve(&state.workspaces, &workspaces);
    let marked = !state.workspaces.remove(&anchor.workspace_id);
    if marked {
        state.workspaces.insert(anchor.workspace_id.clone());
    }
    publish(client, state_path, state, &workspaces)?;
    let body = if marked {
        format!("{} is a priority space", anchor.label)
    } else {
        format!("{} is no longer a priority space", anchor.label)
    };
    client
        .call_value(
            "notification.show",
            &json!({"title": "Space priority", "body": body}),
        )
        .context("show the new priority")?;
    Ok(())
}

/// Saves the marks resolved against `workspaces` and reports every token
/// that differs from them.
fn publish(
    client: &Client,
    state_path: &Path,
    mut state: State,
    workspaces: &[WorkspaceInfo],
) -> Result<()> {
    let marked = resolve(&state.workspaces, workspaces);
    if marked != state.workspaces {
        state.workspaces = marked;
        state.save(state_path).context("save priority spaces")?;
    }
    for (workspace_id, mark) in changes(workspaces, &state.workspaces) {
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

/// Saved marks moved onto their family anchors, without spaces that are gone.
/// A space marked on its own passes its mark to a checkout opened later.
fn resolve(saved: &BTreeSet<String>, workspaces: &[WorkspaceInfo]) -> BTreeSet<String> {
    saved
        .iter()
        .filter_map(|id| {
            workspaces
                .iter()
                .find(|workspace| &workspace.workspace_id == id)
        })
        .map(|workspace| family::anchor(workspaces, workspace).workspace_id.clone())
        .collect()
}

/// Token updates that give each space its family anchor's mark.
fn changes<'a>(
    workspaces: &'a [WorkspaceInfo],
    marked: &BTreeSet<String>,
) -> Vec<(&'a str, Option<&'static str>)> {
    workspaces
        .iter()
        .filter_map(|workspace| {
            let want = marked
                .contains(&family::anchor(workspaces, workspace).workspace_id)
                .then_some(MARK);
            let have = workspace.tokens.get(TOKEN_KEY).map(String::as_str);
            (have != want).then_some((workspace.workspace_id.as_str(), want))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use family::tests::space;

    fn ids(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn marks_move_to_their_anchor_and_gone_spaces_drop() {
        let spaces = [
            space("child", false, Some(("repo", true))),
            space("parent", false, Some(("repo", false))),
            space("plain", false, None),
        ];
        assert_eq!(
            resolve(&ids(&["child", "plain", "closed"]), &spaces),
            ids(&["parent", "plain"])
        );
    }

    #[test]
    fn a_marked_family_marks_every_member_and_clears_the_rest() {
        let spaces = [
            space("parent", true, Some(("repo", false))),
            space("child", false, Some(("repo", true))),
            space("plain", true, None),
            space("other", false, None),
        ];
        assert_eq!(
            changes(&spaces, &ids(&["parent"])),
            [("child", Some(MARK)), ("plain", None)]
        );
        assert_eq!(
            changes(&spaces, &ids(&["parent", "plain"])),
            [("child", Some(MARK))]
        );
    }
}
