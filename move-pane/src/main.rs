//! Move the focused pane to another tab. With one tab the pane goes straight
//! into a new tab; otherwise Herdr's native picker lists the destinations.
use std::{process::ExitCode, time::Duration};

use anyhow::{Context, Result};
use herdr_client::{Client, Environment, PickItem, PickOutcome, PickParams, TabInfo};
use serde_json::json;

const NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(2);
const NEW_TAB: &str = "new-tab";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("move-pane: {error:#}");
            if let Ok(client) = Client::from_env() {
                let _ = client.with_timeout(NOTIFICATION_TIMEOUT).call_value(
                    "notification.show",
                    &json!({"title": "Move pane failed", "body": format!("{error:#}")}),
                );
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let context = Environment::load()?
        .context
        .context("HERDR_PLUGIN_CONTEXT_JSON is not set")?;
    let pane_id = context
        .focused_pane_id
        .context("the invocation names no pane")?;
    let workspace_id = context
        .workspace_id
        .context("the invocation names no space")?;
    let tab_id = context.tab_id.context("the invocation names no tab")?;
    let mut tabs = context.tabs;
    tabs.sort_by_key(|tab| tab.number);
    let destinations = destinations(&workspace_id, &tabs, &tab_id);
    let client = Client::from_env()?;
    let destination = match destinations.as_slice() {
        [only] => only,
        _ => {
            let outcome = client
                .pick(&PickParams {
                    title: "Move pane to".into(),
                    items: destinations.iter().map(Destination::item).collect(),
                    selected: None,
                    create: None,
                    client_id: context.client_id,
                })
                .context("show the destination picker")?;
            let PickOutcome::Picked { id } = outcome else {
                return Ok(());
            };
            destinations
                .iter()
                .find(|destination| destination.id() == id)
                .context("the picked destination is gone")?
        }
    };
    move_pane(&client, &pane_id, destination)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    NewTab { workspace_id: String },
    Tab(TabInfo),
}

impl Destination {
    fn id(&self) -> &str {
        match self {
            Self::NewTab { .. } => NEW_TAB,
            Self::Tab(tab) => &tab.tab_id,
        }
    }

    fn item(&self) -> PickItem {
        match self {
            Self::NewTab { .. } => PickItem::new(NEW_TAB, "New tab"),
            Self::Tab(tab) => PickItem {
                detail: Some(match tab.pane_count {
                    1 => "1 pane".into(),
                    count => format!("{count} panes"),
                }),
                badge: Some(tab.number.to_string()),
                ..PickItem::new(&tab.tab_id, &tab.label)
            },
        }
    }
}

/// A new tab first, then every other tab of the workspace in tab order.
fn destinations(workspace_id: &str, tabs: &[TabInfo], current_tab_id: &str) -> Vec<Destination> {
    std::iter::once(Destination::NewTab {
        workspace_id: workspace_id.to_owned(),
    })
    .chain(
        tabs.iter()
            .filter(|tab| tab.tab_id != current_tab_id)
            .cloned()
            .map(Destination::Tab),
    )
    .collect()
}

fn move_pane(client: &Client, pane_id: &str, destination: &Destination) -> Result<()> {
    let destination = match destination {
        Destination::NewTab { workspace_id } => {
            json!({"type": "new_tab", "workspace_id": workspace_id})
        }
        Destination::Tab(tab) => json!({"type": "tab", "tab_id": tab.tab_id, "split": "right"}),
    };
    client
        .call_value(
            "pane.move",
            &json!({"pane_id": pane_id, "destination": destination, "focus": true}),
        )
        .context("move pane")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(number: usize, label: &str, pane_count: usize) -> TabInfo {
        serde_json::from_value(json!({
            "tab_id": format!("w1:t{number}"), "workspace_id": "w1", "number": number,
            "label": label, "focused": number == 1, "pane_count": pane_count,
            "agent_status": "idle",
        }))
        .unwrap()
    }

    #[test]
    fn destinations_start_with_a_new_tab_and_skip_the_current_one() {
        let tabs = [tab(1, "main", 2), tab(2, "logs", 1), tab(3, "tests", 3)];
        let found = destinations("w1", &tabs, "w1:t2");
        let items: Vec<_> = found.iter().map(Destination::item).collect();
        let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["New tab", "main", "tests"]);
        assert_eq!(found[0].id(), NEW_TAB);
        assert_eq!(found[2].id(), "w1:t3");
        assert_eq!(items[2].detail.as_deref(), Some("3 panes"));
        assert_eq!(destinations("w1", &tabs[..1], "w1:t1").len(), 1);
    }
}
