//! Space Priority: marks spaces as priority through the persisted
//! `space_priority` workspace metadata token. The fork's priority order sorts
//! blocked and done agents in those spaces ahead of every other agent;
//! `[[ui.sidebar.spaces.pinned]]` can gather the spaces at the top of the TUI's
//! sidebar, and the token's value doubles as a marker.
//!
//! Herdr saves the token with the session and drops it with its space, so the
//! plugin keeps no state of its own.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herdr_client::{Client, Environment, PluginInvocation, WorkspaceInfo};
use serde_json::json;

const SOURCE_ID: &str = "gjermundgaraba.herdr-space-priority";
/// Workspace metadata key users point the fork's generic options at.
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
    let client = Client::from_env()?.with_timeout(SOCKET_TIMEOUT);
    let context = environment.context.clone().unwrap_or_default();
    match environment.invocation() {
        Some(PluginInvocation::Action(_)) => {
            let workspaces = client.list_workspaces().context("list the spaces")?;
            toggle(&client, context.workspace_id.as_deref(), &workspaces)
        }
        _ => bail!("run this through the plugin's action"),
    }
}

/// Flips the mark of the invocation's space.
fn toggle(client: &Client, workspace_id: Option<&str>, workspaces: &[WorkspaceInfo]) -> Result<()> {
    let workspace_id = workspace_id.context("the invocation names no space")?;
    let target = workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == workspace_id)
        .context("the space is gone")?;
    let marked = !target.tokens.contains_key(TOKEN_KEY);
    publish(client, &target.workspace_id, marked)?;
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

fn publish(client: &Client, workspace_id: &str, marked: bool) -> Result<()> {
    client
        .call_value(
            "workspace.report_metadata",
            &json!({
                "workspace_id": workspace_id,
                "source": SOURCE_ID,
                "tokens": {TOKEN_KEY: marked.then_some(MARK)},
                "persist": true,
            }),
        )
        .with_context(|| format!("publish the priority of {workspace_id}"))?;
    Ok(())
}
