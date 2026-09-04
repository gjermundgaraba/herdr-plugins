use anyhow::{Result, anyhow};
use herdr_client::AgentInfo;
use serde_json::{Value, json};

use crate::PLUGIN_ID;

pub const HERDR_LAYER: usize = 2;

pub type Caller<'a> = dyn Fn(&str, Value) -> Result<Value> + 'a;

pub fn focus_agent(call: &Caller<'_>, pane_id: &str) -> Result<()> {
    call("agent.focus", json!({ "target": pane_id }))?;
    Ok(())
}

pub fn focus_pane(call: &Caller<'_>, pane_id: &str, direction: &str) -> Result<()> {
    call(
        "pane.focus_direction",
        json!({ "pane_id": pane_id, "direction": direction }),
    )?;
    Ok(())
}

pub fn prompt(call: &Caller<'_>, prompt: &str, submit: bool, agent: &AgentInfo) -> Result<()> {
    let pane = &agent.pane_id;
    if submit {
        call("agent.prompt", json!({ "target": pane, "text": prompt }))?;
    } else {
        call("pane.send_text", json!({ "pane_id": pane, "text": prompt }))?;
    }
    Ok(())
}

pub fn submit(call: &Caller<'_>, agent: &AgentInfo) -> Result<()> {
    call(
        "agent.send_keys",
        json!({ "target": &agent.pane_id, "keys": ["enter"] }),
    )?;
    Ok(())
}

pub fn open_diff(call: &Caller<'_>, agent: &AgentInfo) -> Result<()> {
    let cwd = agent
        .foreground_cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty())
        .or(agent.cwd.as_deref().filter(|cwd| !cwd.is_empty()))
        .ok_or_else(|| anyhow!("focused agent has no repository context"))?;
    call(
        "plugin.pane.open",
        json!({
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
    use std::cell::RefCell;

    use super::*;
    use serde_json::json;

    #[test]
    fn layer_identity_matches_the_plugin_id() {
        assert_eq!(layer_identity(2).process, format!("{PLUGIN_ID}.layer-2"));
    }

    #[test]
    fn actions_forward_exact_methods_and_parameters() {
        let calls = RefCell::new(Vec::new());
        let caller = |method: &str, params: Value| {
            calls.borrow_mut().push((method.to_owned(), params));
            Ok(json!({"type":"ok"}))
        };
        let agent: AgentInfo = serde_json::from_value(json!({
            "terminal_id":"term1", "agent":"codex", "agent_status":"idle",
            "workspace_id":"w1", "tab_id":"w1:t1", "pane_id":"w1:p1",
            "focused":true, "state_change_seq":1, "cwd":"/tmp", "revision":1
        }))
        .unwrap();

        prompt(&caller, "hello", true, &agent).unwrap();
        submit(&caller, &agent).unwrap();
        focus_pane(&caller, &agent.pane_id, "left").unwrap();

        assert_eq!(
            calls.into_inner(),
            [
                (
                    "agent.prompt".into(),
                    json!({"target":"w1:p1", "text":"hello"})
                ),
                (
                    "agent.send_keys".into(),
                    json!({"target":"w1:p1", "keys":["enter"]})
                ),
                (
                    "pane.focus_direction".into(),
                    json!({"pane_id":"w1:p1", "direction":"left"})
                )
            ]
        );
    }
}
