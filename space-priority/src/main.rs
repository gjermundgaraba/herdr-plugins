//! Space Priority: marks spaces as priority through the persisted
//! `space_priority` workspace metadata token. In the fork's priority agent
//! order, blocked and done agents in those spaces sort ahead of every other
//! agent; the token's value doubles as a sidebar marker.
//!
//! Herdr saves the token with the session and drops it with its space, so the
//! plugin keeps no state of its own.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herdr_client::{Client, Environment, PluginInvocation, WorkspaceInfo, socket_scope_dir};
use serde_json::json;

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
    let client = Client::from_env()?.with_timeout(SOCKET_TIMEOUT);
    let context = environment.context.clone().unwrap_or_default();
    match environment.invocation() {
        Some(PluginInvocation::Startup) => {
            import_saved_marks(&environment, &client, &context.workspaces)
        }
        Some(PluginInvocation::Action(_)) => toggle(
            &client,
            context.workspace_id.as_deref(),
            &context.workspaces,
        ),
        _ => bail!("run this through the plugin's action or startup hook"),
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

/// One-shot import of the per-session file earlier versions saved marks in.
/// The file is kept as `priority.json.imported`. Remove this once every
/// session has started with this version.
fn import_saved_marks(
    environment: &Environment,
    client: &Client,
    workspaces: &[WorkspaceInfo],
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Saved {
        #[serde(default)]
        workspaces: BTreeSet<String>,
    }
    let path = legacy_state_path(environment)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("read saved priority spaces"),
    };
    let saved: Saved = serde_json::from_slice(&bytes).context("parse saved priority spaces")?;
    for workspace in workspaces
        .iter()
        .filter(|workspace| saved.workspaces.contains(&workspace.workspace_id))
    {
        publish(client, &workspace.workspace_id, true)?;
    }
    std::fs::rename(&path, path.with_extension("json.imported"))
        .context("retire saved priority spaces")
}

fn legacy_state_path(environment: &Environment) -> Result<PathBuf> {
    let plugin = environment.require_plugin()?;
    let socket = environment
        .socket_path
        .as_deref()
        .context("HERDR_SOCKET_PATH is not set")?;
    Ok(socket_scope_dir(&plugin.data_dir(), socket).join("priority.json"))
}
