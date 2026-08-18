//! Shared Herdr streaming and submit plumbing for the herdr-picker-herdr
//! source and focus executables.

use std::{
    io::{self, Write},
    process::ExitCode,
    thread,
    time::Duration,
};

use herdr_client::{
    AgentStatus, Client, Error as ClientError, EventSubscription, SessionSnapshot, Subscription,
};
use serde::Serialize;
use serde_json::{Value, json};

pub fn run(name: &str, task: impl FnOnce() -> Result<(), String>) -> ExitCode {
    match task() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{name}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Read one picker context from stdin, then stream one item snapshot per
/// session change while the picker stays open.
pub fn serve(items: impl Fn(&SessionSnapshot) -> Vec<Item>) -> Result<(), String> {
    let _: Value = serde_json::from_reader(io::stdin())
        .map_err(|error| format!("invalid picker context: {error}"))?;
    let client = Client::from_env().map_err(|error| format!("cannot connect to Herdr: {error}"))?;
    let mut previous: Option<Vec<Item>> = None;
    let mut publish = |snapshot: &SessionSnapshot| -> Result<(), String> {
        let next = items(snapshot);
        if previous.as_ref() != Some(&next) {
            emit(&next)?;
            previous = Some(next);
        }
        Ok(())
    };

    loop {
        let (snapshot, mut events) = subscribe_events(&client)?;
        let subscribed_panes = pane_ids(&snapshot);
        publish(&snapshot)?;

        loop {
            let Some(_) = events
                .next_event()
                .map_err(|error| format!("cannot read Herdr events: {error}"))?
            else {
                thread::sleep(Duration::from_millis(100));
                break;
            };
            let snapshot = client
                .snapshot()
                .map_err(|error| format!("cannot refresh Herdr session: {error}"))?;
            let topology_changed = pane_ids(&snapshot) != subscribed_panes;
            publish(&snapshot)?;
            if topology_changed {
                break;
            }
        }
    }
}

/// Read the final picker context from stdin and call `method` with the
/// `value_field` of the selected item as its `parameter`.
pub fn submit(method: &str, value_field: &str, parameter: &str) -> Result<(), String> {
    let context: Value = serde_json::from_reader(io::stdin())
        .map_err(|error| format!("invalid picker context: {error}"))?;
    let value = selected_value(&context, value_field)?;
    Client::from_env()
        .map_err(|error| format!("cannot connect to Herdr: {error}"))?
        .call_value(method, &json!({ (parameter): value }))
        .map(|_| ())
        .map_err(|error| format!("{method} failed: {error}"))
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Item {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub badge: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub indicator: String,
    pub tone: &'static str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub spinning: bool,
    pub search: String,
    pub value: Value,
}

pub fn presentation(status: &AgentStatus) -> (&'static str, &'static str, bool) {
    match status.as_str() {
        AgentStatus::BLOCKED => ("◉", "danger", false),
        AgentStatus::DONE => ("●", "accent", false),
        AgentStatus::WORKING => ("", "warning", true),
        AgentStatus::IDLE => ("✓", "success", false),
        _ => ("○", "muted", false),
    }
}

fn lifecycle_subscriptions() -> impl Iterator<Item = EventSubscription> {
    const EVENTS: &[&str] = &[
        "workspace.created",
        "workspace.updated",
        "workspace.renamed",
        "workspace.moved",
        "workspace.reordered",
        "workspace.closed",
        "worktree.created",
        "worktree.opened",
        "worktree.removed",
        "tab.created",
        "tab.closed",
        "tab.renamed",
        "tab.moved",
        "pane.created",
        "pane.closed",
        "pane.updated",
        "pane.moved",
        "pane.exited",
        "pane.agent_detected",
    ];
    EVENTS.iter().map(|event| EventSubscription::new(*event))
}

fn subscribe_events(client: &Client) -> Result<(SessionSnapshot, Subscription), String> {
    loop {
        let before = client
            .snapshot()
            .map_err(|error| format!("cannot load Herdr session: {error}"))?;
        let subscriptions = lifecycle_subscriptions()
            .chain(before.panes.iter().map(|pane| {
                EventSubscription::new("pane.agent_status_changed")
                    .filter("pane_id", pane.pane_id.clone())
            }))
            .collect::<Vec<_>>();
        let events = match client.subscribe(&subscriptions) {
            Ok(events) => events,
            Err(ClientError::Api(error)) if error.code == "pane_not_found" => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => {
                return Err(format!("cannot subscribe to Herdr events: {error}"));
            }
        };
        let after = client
            .snapshot()
            .map_err(|error| format!("cannot refresh Herdr session: {error}"))?;
        if pane_ids(&before) == pane_ids(&after) {
            return Ok((after, events));
        }
    }
}

fn pane_ids(snapshot: &SessionSnapshot) -> Vec<String> {
    let mut ids = snapshot
        .panes
        .iter()
        .map(|pane| pane.pane_id.clone())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

#[derive(Serialize)]
struct ProviderSnapshot<'a> {
    items: &'a [Item],
}

fn emit(items: &[Item]) -> Result<(), String> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &ProviderSnapshot { items })
        .map_err(|error| format!("cannot encode provider message: {error}"))?;
    stdout
        .write_all(b"\n")
        .and_then(|_| stdout.flush())
        .map_err(|error| format!("cannot write provider message: {error}"))
}

fn selected_value<'a>(context: &'a Value, value_field: &str) -> Result<&'a str, String> {
    let step = context["step"]
        .as_str()
        .ok_or_else(|| "picker context step is missing".to_string())?;
    context["selections"][step]["value"][value_field]
        .as_str()
        .ok_or_else(|| format!("selections.{step}.value.{value_field} is missing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_reads_the_final_compact_selection() {
        let context = json!({
            "step": "agent",
            "selections": {
                "agent": {
                    "id": "w1:p1",
                    "value": { "pane_id": "w1:p1" }
                }
            }
        });

        assert_eq!(selected_value(&context, "pane_id").unwrap(), "w1:p1");
    }
}
