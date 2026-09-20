use anyhow::Result;
use herdr_client::frontend::{Agent, Input};
use serde_json::{Value, json};

pub const HERDR_LAYER: usize = 2;

/// One request Micro sends through the captured TUI route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Navigate {
        pane_id: String,
    },
    Input(Input),
    /// An endpoint method advertised on the TUI's client command lane.
    Call {
        method: String,
        params: Value,
    },
}

impl Call {
    fn method(method: &str, params: Value) -> Self {
        Self::Call {
            method: method.into(),
            params,
        }
    }
}

pub type Caller<'a> = dyn Fn(Call) -> Result<()> + 'a;

pub fn focus_pane(call: &Caller<'_>, pane_id: &str, direction: &str) -> Result<()> {
    call(Call::method(
        "pane.focus_direction",
        json!({"pane_id": pane_id, "direction": direction}),
    ))
}

/// A submitted prompt targets the captured agent pane; unsubmitted text is
/// ordinary input and follows TUI focus.
pub fn prompt(call: &Caller<'_>, prompt: &str, submit: bool, agent: &Agent) -> Result<()> {
    if submit {
        call(Call::method(
            "agent.prompt",
            json!({"target": agent.pane_id, "text": prompt}),
        ))
    } else {
        call(Call::Input(Input::Text(prompt.into())))
    }
}

pub fn submit(call: &Caller<'_>) -> Result<()> {
    call(Call::Input(Input::Keys(vec!["enter".into()])))
}

pub use codex_micro::service::layer_identity;
#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::PLUGIN_ID;

    #[test]
    fn layer_identity_matches_the_plugin_id() {
        assert_eq!(layer_identity(2).process, format!("{PLUGIN_ID}.layer-2"));
    }

    #[test]
    fn actions_forward_exact_calls() {
        let calls = RefCell::new(Vec::new());
        let caller = |call: Call| {
            calls.borrow_mut().push(call);
            Ok(())
        };
        let agent: Agent = serde_json::from_value(json!({
            "agent":"codex", "agent_status":"idle", "workspace_id":"w1", "tab_id":"w1:t1",
            "pane_id":"w1:p1", "focused":true, "state_change_seq":1,
            "state_labels":[], "tokens":[]
        }))
        .unwrap();

        prompt(&caller, "hello", true, &agent).unwrap();
        prompt(&caller, "draft", false, &agent).unwrap();
        submit(&caller).unwrap();
        focus_pane(&caller, &agent.pane_id, "left").unwrap();

        assert_eq!(
            calls.into_inner(),
            [
                Call::method("agent.prompt", json!({"target":"w1:p1","text":"hello"})),
                Call::Input(Input::Text("draft".into())),
                Call::Input(Input::Keys(vec!["enter".into()])),
                Call::method(
                    "pane.focus_direction",
                    json!({"pane_id":"w1:p1","direction":"left"})
                ),
            ]
        );
    }
}
