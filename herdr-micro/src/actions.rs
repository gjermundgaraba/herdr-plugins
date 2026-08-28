use anyhow::{Result, anyhow};
use herdr_client::{AgentInfo, Client, PaneInfo, PaneLayoutSnapshot};
use serde_json::json;

use crate::PLUGIN_ID;

pub const GHOSTTY_PROCESS: &str = "com.mitchellh.ghostty";
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

#[derive(Debug, Clone, PartialEq)]
pub struct ScrollPlan {
    pub pane_id: String,
    pub notches: i64,
    pub column: f64,
    pub row: f64,
}

pub fn scroll_plan(
    pane: &PaneInfo,
    layout: &PaneLayoutSnapshot,
    direction: &str,
    percent: f64,
) -> Result<ScrollPlan> {
    let pane_id = pane.pane_id.as_str();
    let rows = pane
        .scroll
        .map(|scroll| scroll.viewport_rows)
        .filter(|rows| *rows >= 1)
        .ok_or_else(|| anyhow!("focused pane dimensions unavailable"))?;
    let rect = layout
        .panes
        .iter()
        .find(|entry| entry.pane_id == pane_id)
        .map(|entry| entry.rect)
        .ok_or_else(|| anyhow!("focused pane dimensions unavailable"))?;
    let (x, y, width, height) = (
        f64::from(rect.x),
        f64::from(rect.y),
        f64::from(rect.width),
        f64::from(rect.height),
    );
    let notches = ((rows as f64 * percent) / 300.0).round().max(1.0) as i64;
    Ok(ScrollPlan {
        pane_id: pane_id.into(),
        notches: if direction == "up" { notches } else { -notches },
        column: x + width / 2.0,
        row: y + height / 2.0,
    })
}

pub use codex_micro::service::layer_identity;
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn layer_identity_matches_the_plugin_id() {
        assert_eq!(layer_identity(2).process, format!("{PLUGIN_ID}.layer-2"));
    }

    #[test]
    fn computes_scroll_geometry() {
        let pane: PaneInfo = serde_json::from_value(json!({
            "pane_id":"w1:p1", "terminal_id":"term1", "workspace_id":"w1",
            "tab_id":"w1:t1", "focused":true, "agent_status":"idle", "revision":1,
            "scroll":{"offset_from_bottom":0,"max_offset_from_bottom":100,"viewport_rows":71}
        }))
        .unwrap();
        let layout: PaneLayoutSnapshot = serde_json::from_value(json!({
            "workspace_id":"w1", "tab_id":"w1:t1", "zoomed":false,
            "area":{"x":32,"y":1,"width":250,"height":73}, "focused_pane_id":"w1:p1",
            "panes":[{"pane_id":"w1:p1","focused":true,"rect":{"x":32,"y":1,"width":125,"height":73}}],
            "splits":[]
        })).unwrap();
        let plan = scroll_plan(&pane, &layout, "up", 50.0).unwrap();
        assert_eq!(plan.notches, 12);
        assert_eq!(plan.column, 94.5);
        assert_eq!(plan.row, 37.5);
    }
}
