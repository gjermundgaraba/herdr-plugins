use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::thread;
use std::time::Duration;

use crate::{
    config::{AgentStatus, EffortConfig},
    PLUGIN_ID,
};

pub const CHATGPT_BUNDLE_IDS: [&str; 2] = ["com.openai.codex", "com.openai.chat"];
pub const GHOSTTY_PROCESS: &str = "com.mitchellh.ghostty";
pub const HERDR_LAYER: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub terminal_id: String,
    pub pane_id: String,
    pub agent: String,
    pub agent_status: AgentStatus,
    pub state_change_seq: u64,
    pub focused: bool,
    pub cwd: String,
}

pub fn normalize_agent_status(status: Option<&str>) -> AgentStatus {
    match status {
        Some("idle") => AgentStatus::Idle,
        Some("working") => AgentStatus::Working,
        Some("blocked") => AgentStatus::Blocked,
        Some("done") => AgentStatus::Done,
        _ => AgentStatus::Unknown,
    }
}

pub fn parse_agents(value: &Value) -> Result<Vec<Agent>> {
    let agents = value
        .pointer("/result/agents")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Herdr returned invalid agent list"))?;
    agents
        .iter()
        .map(|raw| {
            let terminal_id = string_field(raw, "terminal_id")?
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("Herdr returned invalid agent list"))?;
            let pane_id = string_field(raw, "pane_id")?
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("Herdr returned invalid agent list"))?;
            let agent = string_field(raw, "agent")?.unwrap_or_default();
            let agent_status = string_field(raw, "agent_status")?;
            let cwd = string_field(raw, "foreground_cwd")?
                .filter(|value| !value.is_empty())
                .or(string_field(raw, "cwd")?)
                .unwrap_or_default();
            Ok(Agent {
                terminal_id: terminal_id.into(),
                pane_id: pane_id.into(),
                agent: agent.into(),
                agent_status: normalize_agent_status(agent_status),
                state_change_seq: raw
                    .get("state_change_seq")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                focused: raw.get("focused").and_then(Value::as_bool) == Some(true),
                cwd: cwd.into(),
            })
        })
        .collect()
}

fn string_field<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => bail!("Herdr returned non-string {key}"),
    }
}

fn pane(agent: &Agent) -> Result<&str> {
    if agent.pane_id.is_empty() {
        bail!("focused agent has no pane");
    }
    Ok(&agent.pane_id)
}

pub fn prompt_args(prompt: &str, submit: bool, agent: &Agent) -> Result<Vec<String>> {
    let pane = pane(agent)?;
    Ok(if submit {
        vec!["agent", "prompt", pane, prompt]
    } else {
        vec!["pane", "send-text", pane, prompt]
    }
    .into_iter()
    .map(str::to_owned)
    .collect())
}

pub fn submit_args(agent: &Agent) -> Result<Vec<String>> {
    Ok(vec!["agent", "send-keys", pane(agent)?, "enter"]
        .into_iter()
        .map(str::to_owned)
        .collect())
}

pub fn diff_pane_args(agent: &Agent) -> Result<Vec<String>> {
    if agent.cwd.is_empty() {
        bail!("focused agent has no repository context");
    }
    Ok(vec![
        "plugin",
        "pane",
        "open",
        "--plugin",
        PLUGIN_ID,
        "--entrypoint",
        "diff",
        "--cwd",
        &agent.cwd,
        "--focus",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect())
}

pub fn fast_mode_plan(agent: &Agent) -> Result<Vec<Vec<String>>> {
    let pane = pane(agent)?;
    match agent.agent.as_str() {
        "codex" => Ok(vec![vec!["agent", "prompt", pane, "/fast"]]),
        "pi" => Ok(vec![
            vec!["pane", "send-text", pane, "/fast"],
            vec!["agent", "send-keys", pane, "enter"],
        ]),
        other => bail!(
            "unsupported focused agent: {}",
            if other.is_empty() { "none" } else { other }
        ),
    }
    .map(|plan| {
        plan.into_iter()
            .map(|args| args.into_iter().map(str::to_owned).collect())
            .collect()
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortStep {
    pub args: Vec<String>,
    pub wait_after_ms: Option<u64>,
}

pub fn plan_effort_change(
    agent: &str,
    direction: &str,
    pane_id: &str,
    config: &EffortConfig,
) -> Result<Vec<EffortStep>> {
    if !matches!(direction, "raise" | "lower") {
        bail!("direction must be raise or lower, got {direction}");
    }
    if pane_id.is_empty() {
        bail!("focused Herdr pane is required");
    }
    let step = |args: Vec<&str>, wait_after_ms| EffortStep {
        args: args.into_iter().map(str::to_owned).collect(),
        wait_after_ms,
    };
    match agent {
        "codex" => {
            let key = match direction {
                "raise" => config.codex.raise.as_deref(),
                _ => config.codex.lower.as_deref(),
            }
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| anyhow!("Codex {direction} effort shortcut is not configured"))?;
            Ok(vec![step(vec!["pane", "send-keys", pane_id, key], None)])
        }
        "claude" => Ok(vec![
            step(vec!["pane", "send-text", pane_id, "/effort"], None),
            step(vec!["pane", "send-keys", pane_id, "enter"], Some(150)),
            step(
                vec![
                    "pane",
                    "send-keys",
                    pane_id,
                    if direction == "raise" {
                        "right"
                    } else {
                        "left"
                    },
                ],
                Some(100),
            ),
            step(vec!["pane", "send-keys", pane_id, "enter"], None),
        ]),
        "pi" => Ok(vec![step(
            vec![
                "pane",
                "send-keys",
                pane_id,
                if direction == "raise" {
                    "ctrl+shift+right"
                } else {
                    "ctrl+shift+left"
                },
            ],
            None,
        )]),
        other => bail!(
            "unsupported focused agent: {}",
            if other.is_empty() { "none" } else { other }
        ),
    }
}

pub fn execute_effort_plan<F>(bin: &str, plan: &[EffortStep], mut run: F) -> Result<()>
where
    F: FnMut(&str, &[String]) -> Result<()>,
{
    for step in plan {
        run(bin, &step.args)?;
        if let Some(ms) = step.wait_after_ms {
            thread::sleep(Duration::from_millis(ms));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScrollPlan {
    pub pane_id: String,
    pub notches: i64,
    pub x: f64,
    pub y: f64,
}

pub fn scroll_plan(
    pane: &Value,
    layout: &Value,
    direction: &str,
    percent: f64,
) -> Result<ScrollPlan> {
    let pane_id = pane
        .get("pane_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("focused pane dimensions unavailable"))?;
    let rows = pane
        .pointer("/scroll/viewport_rows")
        .and_then(Value::as_i64)
        .filter(|rows| *rows >= 1)
        .ok_or_else(|| anyhow!("focused pane dimensions unavailable"))?;
    let rect = layout
        .get("panes")
        .and_then(Value::as_array)
        .and_then(|panes| {
            panes
                .iter()
                .find(|entry| entry.get("pane_id").and_then(Value::as_str) == Some(pane_id))
        })
        .and_then(|entry| entry.get("rect"))
        .ok_or_else(|| anyhow!("focused pane dimensions unavailable"))?;
    let area = layout
        .get("area")
        .ok_or_else(|| anyhow!("focused pane dimensions unavailable"))?;
    let number = |value: &Value, key| {
        value
            .get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow!("focused pane dimensions unavailable"))
    };
    let (x, y, width, height) = (
        number(rect, "x")?,
        number(rect, "y")?,
        number(rect, "width")?,
        number(rect, "height")?,
    );
    let (area_x, area_y, area_width, area_height) = (
        number(area, "x")?,
        number(area, "y")?,
        number(area, "width")?,
        number(area, "height")?,
    );
    let notches = ((rows as f64 * percent) / 300.0).round().max(1.0) as i64;
    Ok(ScrollPlan {
        pane_id: pane_id.into(),
        notches: if direction == "up" { notches } else { -notches },
        x: (x + width / 2.0 - area_x) / area_width,
        y: (y + height / 2.0 - area_y) / area_height,
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
    use crate::config::default_effort;
    use serde_json::json;

    fn agent(kind: &str) -> Agent {
        Agent {
            terminal_id: "t1".into(),
            pane_id: "p1".into(),
            agent: kind.into(),
            agent_status: AgentStatus::Idle,
            state_change_seq: 0,
            focused: true,
            cwd: "/repo".into(),
        }
    }
    #[test]
    fn plans_actions_and_effort() {
        assert_eq!(
            prompt_args("/model", true, &agent("codex")).unwrap(),
            vec!["agent", "prompt", "p1", "/model"]
        );
        assert_eq!(
            fast_mode_plan(&agent("pi")).unwrap()[1],
            vec!["agent", "send-keys", "p1", "enter"]
        );
        assert_eq!(
            plan_effort_change("claude", "lower", "p2", &default_effort()).unwrap()[2].args,
            vec!["pane", "send-keys", "p2", "left"]
        );
        assert!(plan_effort_change("codex", "raise", "p1", &default_effort()).is_err());
    }
    #[test]
    fn known_chatgpt_bundles_select_layer_one() {
        for process in CHATGPT_BUNDLE_IDS {
            assert_eq!(automatic_layer(Some(process), None), Some(1));
        }
    }
    #[test]
    fn rejects_non_string_agent_fields() {
        assert!(parse_agents(&json!({"result":{"agents":[{
            "terminal_id": 7,
            "pane_id": "p1"
        }]}}))
        .is_err());
    }
    #[test]
    fn computes_scroll_geometry() {
        let plan = scroll_plan(&json!({"pane_id":"w1:p1","scroll":{"viewport_rows":71}}), &json!({"area":{"x":32,"y":1,"width":250,"height":73},"panes":[{"pane_id":"w1:p1","rect":{"x":32,"y":1,"width":125,"height":73}}]}), "up", 50.0).unwrap();
        assert_eq!(plan.notches, 12);
        assert_eq!(plan.x, 0.25);
        assert_eq!(plan.y, 0.5);
    }
}
