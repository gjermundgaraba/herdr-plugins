//! Space Groups: assigns spaces to named groups through the `space_group`
//! workspace metadata token, which the fork's sidebar turns into collapsible
//! headers, and moves them into place so the server order matches the sidebar.
//! A space moved by other means, such as a sidebar drag, takes the group of
//! where it landed.
//!
//! Herdr saves the token with the session and drops it with its space, so the
//! tokens are the only state.
mod order;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herdr_client::{
    Client, Environment, PickCreate, PickItem, PickOutcome, PickParams, PluginContext,
    PluginInvocation, WorkspaceInfo, socket_scope_dir,
};
use serde_json::json;

const SOURCE_ID: &str = "gjermundgaraba.herdr-space-groups";
/// Workspace metadata key the fork's sidebar groups by.
const TOKEN_KEY: &str = "space_group";
/// Picker item id for clearing the group; group names are never empty.
const REMOVE: &str = "";
/// Longest group name accepted from the picker.
const MAX_GROUP_LEN: usize = 40;
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
    let client = Client::from_env()?.with_timeout(SOCKET_TIMEOUT);
    let context = environment.context.clone().unwrap_or_default();
    match environment.invocation() {
        Some(PluginInvocation::Startup) => {
            import_saved_groups(&environment, &client, &context.workspaces)
        }
        Some(PluginInvocation::Event {
            name: "workspace.moved" | "workspace.reordered",
            event,
        }) => regroup_moved(&client, &event.data),
        Some(PluginInvocation::Action(_)) => assign(&client, &context),
        _ => bail!("run this through the plugin's action, startup hook, or event hook"),
    }
}

fn report(client: &Client, workspace_id: &str, group: Option<&str>) -> Result<()> {
    client
        .call_value(
            "workspace.report_metadata",
            &json!({
                "workspace_id": workspace_id,
                "source": SOURCE_ID,
                "tokens": {TOKEN_KEY: group},
                "persist": true,
            }),
        )
        .with_context(|| format!("publish the group of {workspace_id}"))?;
    Ok(())
}

/// One-shot import of the per-session file earlier versions saved groups in.
/// The file is kept as `groups.json.imported`. Remove this once every session
/// has started with this version.
fn import_saved_groups(
    environment: &Environment,
    client: &Client,
    workspaces: &[WorkspaceInfo],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Saved {
        #[serde(default)]
        workspaces: BTreeMap<String, String>,
    }
    let plugin = environment.require_plugin()?;
    let socket = environment
        .socket_path
        .as_deref()
        .context("HERDR_SOCKET_PATH is not set")?;
    let path: PathBuf = socket_scope_dir(&plugin.data_dir(), socket).join("groups.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("read saved groups"),
    };
    let saved: Saved = serde_json::from_slice(&bytes).context("parse saved groups")?;
    for (workspace_id, group) in &saved.workspaces {
        if workspaces
            .iter()
            .any(|workspace| &workspace.workspace_id == workspace_id)
        {
            report(client, workspace_id, Some(group))?;
        }
    }
    std::fs::rename(&path, path.with_extension("json.imported")).context("retire saved groups")
}

/// Asks for the group of the invocation's space in Herdr's native picker.
fn assign(client: &Client, context: &PluginContext) -> Result<()> {
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
    let current = group_of(anchor).map(str::to_owned);
    let group = match client
        .pick(&pick_params(
            title,
            &groups(workspaces),
            current.as_deref(),
            context.client_id,
        ))
        .context("show the group picker")?
    {
        PickOutcome::Picked { id } if id == REMOVE => None,
        PickOutcome::Picked { id: group } => Some(group),
        PickOutcome::Created { text } => match normalize_group(&text) {
            Some(group) => Some(group),
            None => return Ok(()),
        },
        PickOutcome::Cancelled => return Ok(()),
    };
    if group == current {
        return Ok(());
    }
    // Only the anchor carries the family's group.
    for id in &family[1..] {
        let member = workspaces
            .iter()
            .find(|workspace| &workspace.workspace_id == id);
        if member.and_then(group_of).is_some() {
            report(client, id, None)?;
        }
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
fn regroup_moved(client: &Client, data: &serde_json::Value) -> Result<()> {
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
    for anchor in anchors {
        if let Some(group) = order::regroup(&workspaces, anchor) {
            report(client, anchor, group.as_deref())?;
        }
    }
    Ok(())
}

fn group_of(workspace: &WorkspaceInfo) -> Option<&str> {
    workspace.tokens.get(TOKEN_KEY).map(String::as_str)
}

/// Distinct group names in use, sorted case-insensitively.
fn groups(workspaces: &[WorkspaceInfo]) -> Vec<String> {
    let mut groups: Vec<String> = workspaces
        .iter()
        .filter_map(group_of)
        .map(str::to_owned)
        .collect();
    groups.sort_by_key(|group| group.to_lowercase());
    groups.dedup();
    groups
}

/// Trims a typed group name, drops control characters, and caps its length.
fn normalize_group(name: &str) -> Option<String> {
    let name: String = name
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_GROUP_LEN)
        .collect();
    let name = name.trim_end().to_owned();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(id: &str, group: Option<&str>) -> WorkspaceInfo {
        serde_json::from_value(json!({
            "workspace_id": id, "number": 1, "label": id, "focused": false,
            "pane_count": 1, "tab_count": 1, "active_tab_id": format!("{id}:t1"),
            "agent_status": "idle",
            "tokens": group.map_or(json!({}), |group| json!({TOKEN_KEY: group})),
        }))
        .unwrap()
    }

    #[test]
    fn groups_are_distinct_and_sorted() {
        let spaces = [
            space("w1", Some("work")),
            space("w2", Some("Play")),
            space("w3", Some("work")),
            space("w4", None),
            space("w5", Some("infra")),
        ];
        assert_eq!(groups(&spaces), ["infra", "Play", "work"]);
    }

    #[test]
    fn group_names_are_trimmed_and_capped() {
        assert_eq!(normalize_group("  work \n"), Some("work".into()));
        assert_eq!(normalize_group(" \t "), None);
        assert_eq!(
            normalize_group(&"x".repeat(60)).map(|name| name.len()),
            Some(MAX_GROUP_LEN)
        );
    }

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
