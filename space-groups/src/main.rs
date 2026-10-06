//! Space Groups: assigns spaces to named groups through the `space_group`
//! workspace metadata token, which the fork's sidebar turns into collapsible
//! headers, and moves them into place so the server order matches the sidebar.
//! A space moved by other means, such as a sidebar drag, takes the group of
//! where it landed.
//!
//! Metadata tokens do not survive a server restart or live handoff, so every
//! assignment is saved per session and republished by the startup hook.
mod order;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herdr_client::{
    Client, Environment, PickCreate, PickItem, PickOutcome, PickParams, PluginContext,
    PluginInvocation, WorkspaceInfo, socket_scope_dir,
};
use serde_json::json;

use state::State;

const SOURCE_ID: &str = "gjermundgaraba.herdr-space-groups";
/// Workspace metadata key the fork's sidebar groups by.
const TOKEN_KEY: &str = "space_group";
/// Picker item id for clearing the group; group names are never empty.
const REMOVE: &str = "";
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
        Some(PluginInvocation::Action(_)) => assign(&client, &state_path, &context),
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

/// Asks for the group of the invocation's space in Herdr's native picker.
fn assign(client: &Client, state_path: &std::path::Path, context: &PluginContext) -> Result<()> {
    let workspace_id = context
        .workspace_id
        .as_deref()
        .context("the invocation names no space")?;
    let workspaces = &context.workspaces;
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
    let group = match client
        .pick(&pick_params(
            title,
            &state.groups(),
            current.as_deref(),
            context.client_id,
        ))
        .context("show the group picker")?
    {
        PickOutcome::Picked { id } if id == REMOVE => None,
        PickOutcome::Picked { id: group } => Some(group),
        PickOutcome::Created { text } => match state::normalize_group(&text) {
            Some(group) => Some(group),
            None => return Ok(()),
        },
        PickOutcome::Cancelled => return Ok(()),
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

/// Existing groups with the current one selected, a create row for new
/// names, and a remove row when the space has a group.
fn pick_params(
    title: String,
    groups: &[String],
    current: Option<&str>,
    client_id: Option<u64>,
) -> PickParams {
    let mut items = groups
        .iter()
        .map(|group| PickItem {
            badge: (Some(group.as_str()) == current).then(|| "●".to_owned()),
            ..PickItem::new(group, group)
        })
        .collect::<Vec<_>>();
    if current.is_some() {
        items.push(PickItem::new(REMOVE, "Remove from group"));
    }
    PickParams {
        title,
        items,
        selected: current.map(str::to_owned),
        create: Some(PickCreate {
            label: "New group".into(),
        }),
        client_id,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_selects_the_current_group_and_offers_removal_only_when_grouped() {
        let groups = ["infra".to_owned(), "work".to_owned()];
        let grouped = pick_params("Group".into(), &groups, Some("work"), Some(3));
        let ids: Vec<_> = grouped.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["infra", "work", REMOVE]);
        assert_eq!(grouped.items[1].badge.as_deref(), Some("●"));
        assert_eq!(grouped.selected.as_deref(), Some("work"));
        assert_eq!(grouped.client_id, Some(3));
        assert!(grouped.create.is_some());

        let ungrouped = pick_params("Group".into(), &groups, None, None);
        assert_eq!(ungrouped.items.len(), 2);
        assert_eq!(ungrouped.selected, None);
    }
}
