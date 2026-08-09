use anyhow::{Result, anyhow, bail};
use herdr_client::{AgentInfo, Client, PaneInfo, PaneLayoutSnapshot};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{PLUGIN_ID, config::EffortConfig};

pub const CHATGPT_BUNDLE_IDS: [&str; 2] = ["com.openai.codex", "com.openai.chat"];
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortStep {
    pub method: &'static str,
    pub params: Value,
    pub wait_after_ms: Option<u64>,
}

pub fn plan_effort_change(
    agent: &str,
    direction: &str,
    pane_id: &str,
    config: &EffortConfig,
    count: usize,
) -> Result<Vec<EffortStep>> {
    if !matches!(direction, "raise" | "lower") {
        bail!("direction must be raise or lower, got {direction}");
    }
    if pane_id.is_empty() {
        bail!("focused Herdr pane is required");
    }
    if count == 0 {
        bail!("effort change count must be nonzero");
    }
    let step = |method, params, wait_after_ms| EffortStep {
        method,
        params,
        wait_after_ms,
    };
    let repeated_keys = |key: &str| json!({ "pane_id": pane_id, "keys": std::iter::repeat_n(key, count).collect::<Vec<_>>() });
    match agent {
        "codex" => {
            let key = match direction {
                "raise" => config.codex.raise.as_deref(),
                _ => config.codex.lower.as_deref(),
            }
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| anyhow!("Codex {direction} effort shortcut is not configured"))?;
            Ok(vec![EffortStep {
                method: "pane.send_keys",
                params: repeated_keys(key),
                wait_after_ms: None,
            }])
        }
        "claude" => Ok(vec![
            step(
                "pane.send_text",
                json!({ "pane_id": pane_id, "text": "/effort" }),
                None,
            ),
            step(
                "pane.send_keys",
                json!({ "pane_id": pane_id, "keys": ["enter"] }),
                Some(150),
            ),
            EffortStep {
                method: "pane.send_keys",
                params: repeated_keys(if direction == "raise" {
                    "right"
                } else {
                    "left"
                }),
                wait_after_ms: Some(100),
            },
            step(
                "pane.send_keys",
                json!({ "pane_id": pane_id, "keys": ["enter"] }),
                None,
            ),
        ]),
        "pi" => Ok(vec![EffortStep {
            method: "pane.send_keys",
            params: repeated_keys(if direction == "raise" {
                "ctrl+shift+right"
            } else {
                "ctrl+shift+left"
            }),
            wait_after_ms: None,
        }]),
        other => bail!(
            "unsupported focused agent: {}",
            if other.is_empty() { "none" } else { other }
        ),
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayerIdentity {
    #[serde(rename = "appName")]
    pub app_name: String,
    pub process: String,
}

pub fn layer_identity(layer: usize) -> LayerIdentity {
    LayerIdentity {
        app_name: format!("Herdr Micro Layer {layer}"),
        process: format!("{PLUGIN_ID}.layer-{layer}"),
    }
}
pub fn automatic_layer(
    frontmost_process: Option<&str>,
    focused_herdr_session: Option<&str>,
) -> Option<usize> {
    if frontmost_process.is_some_and(|process| CHATGPT_BUNDLE_IDS.contains(&process)) {
        Some(1)
    } else if frontmost_process == Some(GHOSTTY_PROCESS) && focused_herdr_session.is_some() {
        Some(HERDR_LAYER)
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EffortConfig, EffortKeys};
    use serde_json::json;

    #[test]
    fn plans_effort_as_socket_calls() {
        let effort = EffortConfig {
            codex: EffortKeys {
                raise: Some("ctrl+shift+right".into()),
                lower: Some("ctrl+shift+left".into()),
            },
        };
        assert_eq!(
            plan_effort_change("claude", "lower", "p2", &effort, 3).unwrap()[2].params,
            json!({"pane_id":"p2", "keys":["left", "left", "left"]})
        );
        assert_eq!(
            plan_effort_change("pi", "raise", "p2", &effort, 2).unwrap()[0].params,
            json!({"pane_id":"p2", "keys":["ctrl+shift+right", "ctrl+shift+right"]})
        );
        assert_eq!(
            plan_effort_change("codex", "raise", "p1", &effort, 2).unwrap()[0].method,
            "pane.send_keys"
        );
        assert!(
            plan_effort_change(
                "codex",
                "raise",
                "p1",
                &EffortConfig {
                    codex: EffortKeys {
                        raise: None,
                        lower: None,
                    },
                },
                1,
            )
            .is_err()
        );
        assert!(plan_effort_change("codex", "raise", "p1", &effort, 0).is_err());
    }
    #[test]
    fn known_chatgpt_bundles_select_layer_one() {
        for process in CHATGPT_BUNDLE_IDS {
            assert_eq!(automatic_layer(Some(process), None), Some(1));
        }
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
