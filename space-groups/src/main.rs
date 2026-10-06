//! Space Groups: assigns spaces to named groups through the `space_group`
//! workspace metadata token, which the fork's sidebar turns into collapsible
//! headers, and moves them into place so the server order matches the sidebar.
//! A space moved by other means, such as a sidebar drag, takes the group of
//! where it landed.
//!
//! Metadata tokens do not survive a server restart or live handoff, so every
//! assignment is saved per session and republished by the startup hook.
mod order;
mod picker;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herdr_client::{Client, Environment, PluginInvocation, WorkspaceInfo, socket_scope_dir};
use serde_json::json;

use picker::Choice;
use state::State;

const SOURCE_ID: &str = "gjermundgaraba.herdr-space-groups";
/// Workspace metadata key the fork's sidebar groups by.
const TOKEN_KEY: &str = "space_group";
const ENV_WORKSPACE: &str = "HERDR_SPACE_GROUPS_WORKSPACE_ID";
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("space-groups: {error:#}");
            if let Ok(client) = Client::from_env() {
                let _ = client.with_timeout(SOCKET_TIMEOUT).call_value(
                    "notification.show",
                    &json!({"title": "Space groups failed", "body": format!("{error:#}")}),
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
    let context = environment.context.clone().unwrap_or_default();
    match environment.invocation() {
        Some(PluginInvocation::Startup) => restore(&client, &state_path, &context.workspaces),
        Some(PluginInvocation::Event {
            name: "workspace.closed",
            event,
        }) => {
            let workspace_id = event.data["workspace_id"]
                .as_str()
                .context("workspace.closed event without workspace_id")?;
            forget(&state_path, workspace_id)
        }
        Some(PluginInvocation::Event {
            name: "workspace.moved" | "workspace.reordered",
            event,
        }) => regroup_moved(&client, &state_path, &event.data),
        Some(PluginInvocation::Action(_)) => open_picker(
            &client,
            &environment,
            &state_path,
            context.workspace_id.as_deref(),
        ),
        Some(PluginInvocation::Pane(_)) => pick(&client, &state_path, &context.workspaces),
        _ => bail!("run this through the plugin's action, startup hook, or event hook"),
    }
}

/// Assignments are scoped to the session socket: workspace ids are per session.
fn state_path(environment: &Environment) -> Result<PathBuf> {
    let plugin = environment.require_plugin()?;
    let socket = environment
        .socket_path
        .as_deref()
        .context("HERDR_SOCKET_PATH is not set")?;
    Ok(socket_scope_dir(&plugin.data_dir(), socket).join("groups.json"))
}

fn report(client: &Client, workspace_id: &str, group: Option<&str>) -> Result<()> {
    client
        .call_value(
            "workspace.report_metadata",
            &json!({
                "workspace_id": workspace_id,
                "source": SOURCE_ID,
                "tokens": {TOKEN_KEY: group},
            }),
        )
        .with_context(|| format!("publish the group of {workspace_id}"))?;
    Ok(())
}

/// Republishes saved groups and drops assignments whose space is gone.
fn restore(
    client: &Client,
    state_path: &std::path::Path,
    workspaces: &[WorkspaceInfo],
) -> Result<()> {
    let mut state = State::load(state_path).context("read saved groups")?;
    let before = state.workspaces.len();
    state.workspaces.retain(|workspace_id, _| {
        workspaces
            .iter()
            .any(|workspace| &workspace.workspace_id == workspace_id)
    });
    if state.workspaces.len() != before {
        state.save(state_path).context("save groups")?;
    }
    for (workspace_id, group) in &state.workspaces {
        report(client, workspace_id, Some(group))?;
    }
    Ok(())
}

fn forget(state_path: &std::path::Path, workspace_id: &str) -> Result<()> {
    let mut state = State::load(state_path).context("read saved groups")?;
    if state.workspaces.remove(workspace_id).is_some() {
        state.save(state_path).context("save groups")?;
    }
    Ok(())
}

/// Opens the picker popup for the invocation's space.
fn open_picker(
    client: &Client,
    environment: &Environment,
    state_path: &std::path::Path,
    workspace_id: Option<&str>,
) -> Result<()> {
    let workspace_id = workspace_id.context("the invocation names no space")?;
    let groups = State::load(state_path)
        .context("read saved groups")?
        .groups();
    let plugin_id = environment
        .plugin_id
        .as_deref()
        .context("HERDR_PLUGIN_ID is not set")?;
    client
        .call_value(
            "plugin.pane.open",
            &json!({
                "plugin_id": plugin_id,
                "entrypoint": "pick",
                "placement": "popup",
                "width": 48,
                "height": (groups.len() + 7).min(20),
                "focus": true,
                "env": {ENV_WORKSPACE: workspace_id},
            }),
        )
        .context("open the group picker")?;
    Ok(())
}

fn pick(client: &Client, state_path: &std::path::Path, workspaces: &[WorkspaceInfo]) -> Result<()> {
    let workspace_id = std::env::var(ENV_WORKSPACE)
        .ok()
        .filter(|id| !id.is_empty())
        .with_context(|| format!("{ENV_WORKSPACE} is not set; run this through the action"))?;
    let target = workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == workspace_id)
        .context("the space is gone")?;
    // A worktree family shares its checkout's group, so the checkout is assigned.
    let anchor = order::anchor(workspaces, target);
    let family = order::family(workspaces, &anchor.workspace_id);
    let title = if family.len() > 1 {
        format!("Group for {} and its worktrees", anchor.label)
    } else {
        format!("Group for {}", anchor.label)
    };
    let mut state = State::load(state_path).context("read saved groups")?;
    let current = state.workspaces.get(&anchor.workspace_id).cloned();

    let mut terminal = ratatui::try_init()?;
    let choice = picker::choose(&mut terminal, &title, &state.groups(), current.as_deref());
    ratatui::restore();
    let group = match choice? {
        None => return Ok(()),
        Some(Choice::Group(group) | Choice::Create(group)) => Some(group),
        Some(Choice::Remove) => None,
    };
    if group == current {
        return Ok(());
    }
    let stale = family[1..]
        .iter()
        .filter(|id| state.workspaces.remove(**id).is_some())
        .collect::<Vec<_>>();
    match &group {
        Some(group) => state
            .workspaces
            .insert(anchor.workspace_id.clone(), group.clone()),
        None => state.workspaces.remove(&anchor.workspace_id),
    };
    state.save(state_path).context("save groups")?;
    for id in stale {
        report(client, id, None)?;
    }
    report(client, &anchor.workspace_id, group.as_deref())?;
    if let Some(before) = order::placement(workspaces, &family, group.as_deref()) {
        client
            .call_value(
                "workspace.move_block",
                &json!({"workspace_ids": family, "before_workspace_id": before}),
            )
            .context("move the space into its group")?;
    }
    Ok(())
}

/// Gives each moved family the group of where it landed; see `order::regroup`.
fn regroup_moved(
    client: &Client,
    state_path: &std::path::Path,
    data: &serde_json::Value,
) -> Result<()> {
    let workspaces: Vec<WorkspaceInfo> = serde_json::from_value(data["workspaces"].clone())
        .context("move event without workspaces")?;
    let moved = match data["workspace_ids"].as_array() {
        Some(ids) => ids.iter().filter_map(|id| id.as_str()).collect::<Vec<_>>(),
        None => data["workspace_id"].as_str().into_iter().collect(),
    };
    let mut anchors = Vec::<&str>::new();
    for id in moved {
        let Some(workspace) = workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == id)
        else {
            continue;
        };
        let anchor = order::anchor(&workspaces, workspace).workspace_id.as_str();
        if !anchors.contains(&anchor) {
            anchors.push(anchor);
        }
    }
    let changes = anchors
        .into_iter()
        .filter_map(|anchor| Some((anchor, order::regroup(&workspaces, anchor)?)))
        .collect::<Vec<_>>();
    if changes.is_empty() {
        return Ok(());
    }
    let mut state = State::load(state_path).context("read saved groups")?;
    for (anchor, group) in &changes {
        match group {
            Some(group) => state.workspaces.insert((*anchor).to_owned(), group.clone()),
            None => state.workspaces.remove(*anchor),
        };
    }
    state.save(state_path).context("save groups")?;
    for (anchor, group) in changes {
        report(client, anchor, group.as_deref())?;
    }
    Ok(())
}
