use anyhow::{Result, anyhow};
use herdr_client::{AgentInfo, Client};
use serde_json::json;

use crate::PLUGIN_ID;

pub const HERDR_LAYER: usize = 2;

pub fn focus_agent(client: &Client, pane_id: &str) -> Result<()> {
    client.call_value("agent.focus", &json!({ "target": pane_id }))?;
    Ok(())
}

pub fn focus_pane(client: &Client, pane_id: &str, direction: &str) -> Result<()> {
    client.call_value(
        "pane.focus_direction",
        &json!({ "pane_id": pane_id, "direction": direction }),
    )?;
    Ok(())
}

pub fn prompt(client: &Client, prompt: &str, submit: bool, agent: &AgentInfo) -> Result<()> {
    let pane = &agent.pane_id;
    if submit {
        client.call_value("agent.prompt", &json!({ "target": pane, "text": prompt }))?;
    } else {
        client.call_value(
            "pane.send_text",
            &json!({ "pane_id": pane, "text": prompt }),
        )?;
    }
    Ok(())
}

pub fn submit(client: &Client, agent: &AgentInfo) -> Result<()> {
    client.call_value(
        "agent.send_keys",
        &json!({ "target": &agent.pane_id, "keys": ["enter"] }),
    )?;
    Ok(())
}

pub fn open_diff(client: &Client, agent: &AgentInfo) -> Result<()> {
    let cwd = agent
        .foreground_cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty())
        .or(agent.cwd.as_deref().filter(|cwd| !cwd.is_empty()))
        .ok_or_else(|| anyhow!("focused agent has no repository context"))?;
    client.call_value(
        "plugin.pane.open",
        &json!({
            "plugin_id": PLUGIN_ID,
            "entrypoint": "diff",
            "cwd": cwd,
            "focus": true,
        }),
    )?;
    Ok(())
}

pub use codex_micro::service::layer_identity;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_identity_matches_the_plugin_id() {
        assert_eq!(layer_identity(2).process, format!("{PLUGIN_ID}.layer-2"));
    }
}
