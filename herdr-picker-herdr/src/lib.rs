//! Shared Herdr streaming and submit plumbing for the herdr-picker-herdr
//! source and focus executables.

use std::{
    io::{self, Write},
    process::ExitCode,
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use herdr_client::{
    AgentStatus, Client, Error as ClientError, EventSubscription, SessionSnapshot, Subscription,
};
use serde::{Serialize, de::IgnoredAny};
use serde_json::{Value, json};

pub fn run(name: &str, result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{name}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Read one picker context from stdin, then stream one item snapshot per
/// session change while the picker stays open.
pub fn serve(items: impl Fn(&SessionSnapshot) -> Vec<Item>) -> Result<()> {
    let _: IgnoredAny = serde_json::from_reader(io::stdin()).context("invalid picker context")?;
    let client = Client::from_env().context("cannot connect to Herdr")?;
    let mut previous = None;
    let mut publish = |snapshot: &SessionSnapshot| -> Result<()> {
        let next = items(snapshot);
        if previous.as_ref() != Some(&next) {
            emit(&next)?;
            previous = Some(next);
        }
        Ok(())
    };

    'resubscribe: loop {
        let (snapshot, mut events) = subscribe_events(&client)?;
        let subscribed_panes = pane_ids(&snapshot);
        publish(&snapshot)?;

        while events
            .next_event()
            .context("cannot read Herdr events")?
            .is_some()
        {
            let snapshot = client.snapshot().context("cannot refresh Herdr session")?;
            publish(&snapshot)?;
            if pane_ids(&snapshot) != subscribed_panes {
                continue 'resubscribe;
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Read the final picker context from stdin and call `method` with the
/// `value_field` of the selected item as its `parameter`.
pub fn submit(method: &str, value_field: &str, parameter: &str) -> Result<()> {
    let context: Value = serde_json::from_reader(io::stdin()).context("invalid picker context")?;
    let value = selected_value(&context, value_field)?;
    Client::from_env()
        .context("cannot connect to Herdr")?
        .call_value(method, &json!({ (parameter): value }))
        .with_context(|| format!("{method} failed"))?;
    Ok(())
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
    #[serde(skip_serializing_if = "str::is_empty")]
    pub indicator: &'static str,
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
    EVENTS.iter().copied().map(EventSubscription::new)
}

fn subscribe_events(client: &Client) -> Result<(SessionSnapshot, Subscription)> {
    loop {
        let before = client.snapshot().context("cannot load Herdr session")?;
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
                return Err(error).context("cannot subscribe to Herdr events");
            }
        };
        let after = client.snapshot().context("cannot refresh Herdr session")?;
        if pane_ids(&before) == pane_ids(&after) {
            return Ok((after, events));
        }
    }
}

fn pane_ids(snapshot: &SessionSnapshot) -> Vec<&str> {
    let mut ids = snapshot
        .panes
        .iter()
        .map(|pane| pane.pane_id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

#[derive(Serialize)]
struct ProviderSnapshot<'a> {
    items: &'a [Item],
}

fn emit(items: &[Item]) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &ProviderSnapshot { items })
        .context("cannot encode provider message")?;
    stdout
        .write_all(b"\n")
        .and_then(|_| stdout.flush())
        .context("cannot write provider message")
}

fn selected_value<'a>(context: &'a Value, value_field: &str) -> Result<&'a str> {
    let step = context["step"]
        .as_str()
        .context("picker context step is missing")?;
    context["selections"][step]["value"][value_field]
        .as_str()
        .with_context(|| format!("selections.{step}.value.{value_field} is missing"))
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
