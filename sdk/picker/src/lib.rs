//! Shared wire types and Herdr plumbing for `herdr-picker` examples.

use std::{
    collections::HashSet,
    fmt,
    io::{self, Write},
    process::ExitCode,
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use herdr_hub_client::{AgentStatus, HubClient, Model, ServerMessage};
use serde::{Deserialize, Serialize, de::IgnoredAny};
use serde_json::{Value, json};

const CALL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Muted,
    Accent,
    Success,
    Warning,
    Danger,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<Tone>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub spinning: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub search: String,
    pub value: Value,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot<T = Vec<Item>> {
    pub items: T,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemsError {
    EmptyId { index: usize },
    EmptyTitle { index: usize },
    DuplicateId { id: String },
}

impl fmt::Display for ItemsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId { index } => write!(f, "items[{index}].id must not be empty"),
            Self::EmptyTitle { index } => write!(f, "items[{index}].title must not be empty"),
            Self::DuplicateId { id } => write!(f, "duplicate item id {id:?}"),
        }
    }
}

impl std::error::Error for ItemsError {}

pub fn validate_items(items: &[Item]) -> Result<(), ItemsError> {
    let mut ids = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        if item.id.trim().is_empty() {
            return Err(ItemsError::EmptyId { index });
        }
        if item.title.trim().is_empty() {
            return Err(ItemsError::EmptyTitle { index });
        }
        if !ids.insert(&item.id) {
            return Err(ItemsError::DuplicateId {
                id: item.id.clone(),
            });
        }
    }
    Ok(())
}

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
/// hub model change while the picker stays open.
pub fn serve(items: impl Fn(&Model) -> Vec<Item>) -> Result<()> {
    let _: IgnoredAny = serde_json::from_reader(io::stdin()).context("invalid picker context")?;
    let (events_tx, events_rx) = mpsc::sync_channel(1);
    thread::spawn(move || HubClient::new().run(move |event| events_tx.send(event).is_ok()));

    consume(events_rx, items, emit)?;
    bail!("hub event stream stopped")
}

/// Read the final picker context from stdin and call `method` with the
/// `value_field` of the selected item as its `parameter`.
pub fn submit(method: &str, value_field: &str, parameter: &str) -> Result<()> {
    let context: Value = serde_json::from_reader(io::stdin()).context("invalid picker context")?;
    let (session, value) = selected_value(&context, value_field)?;
    HubClient::new()
        .call(session, method, json!({ (parameter): value }), CALL_TIMEOUT)
        .with_context(|| format!("{method} failed"))?;
    Ok(())
}

pub fn presentation(status: &AgentStatus) -> (&'static str, Tone, bool) {
    match status.as_str() {
        AgentStatus::BLOCKED => ("◉", Tone::Danger, false),
        AgentStatus::DONE => ("●", Tone::Accent, false),
        AgentStatus::WORKING => ("", Tone::Warning, true),
        AgentStatus::IDLE => ("✓", Tone::Success, false),
        _ => ("○", Tone::Muted, false),
    }
}

fn consume(
    events: impl IntoIterator<Item = herdr_hub_client::Result<ServerMessage>>,
    items: impl Fn(&Model) -> Vec<Item>,
    mut output: impl FnMut(&[Item]) -> Result<()>,
) -> Result<()> {
    let mut model = None;
    let mut previous = None;
    let mut outage = false;

    for event in events {
        match event {
            Ok(ServerMessage::Hello {
                protocol: _,
                model: replacement,
            }) => {
                model = Some(replacement);
                outage = false;
            }
            Ok(message) => {
                let Some(model) = model.as_mut() else {
                    continue;
                };
                HubClient::apply(model, &message);
            }
            Err(_) => {
                model = None;
                if !outage {
                    output(&[])?;
                    previous = Some(Vec::new());
                    outage = true;
                }
                continue;
            }
        }

        let Some(model) = model.as_ref() else {
            continue;
        };
        let next = items(model);
        if previous.as_ref() != Some(&next) {
            output(&next)?;
            previous = Some(next);
        }
    }
    Ok(())
}

fn emit(items: &[Item]) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &Snapshot { items })
        .context("cannot encode provider message")?;
    stdout
        .write_all(b"\n")
        .and_then(|_| stdout.flush())
        .context("cannot write provider message")
}

fn selected_value<'a>(context: &'a Value, value_field: &str) -> Result<(&'a str, &'a str)> {
    let step = context["step"]
        .as_str()
        .context("picker context step is missing")?;
    let value = &context["selections"][step]["value"];
    let session = value["session"]
        .as_str()
        .with_context(|| format!("selections.{step}.value.session is missing"))?;
    let selected = value[value_field]
        .as_str()
        .with_context(|| format!("selections.{step}.value.{value_field} is missing"))?;
    Ok((session, selected))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(active: Option<&str>, version: u64) -> Model {
        Model {
            version,
            active: active.map(str::to_owned),
            hosts: Vec::new(),
            sessions: Vec::new(),
        }
    }

    fn active_item(model: &Model) -> Vec<Item> {
        model
            .active
            .iter()
            .map(|active| Item {
                id: active.clone(),
                title: active.clone(),
                value: Value::Null,
                ..Item::default()
            })
            .collect()
    }

    #[test]
    fn submit_reads_the_final_compact_selection() {
        let context = json!({
            "step": "agent",
            "selections": {
                "agent": {
                    "id": "w1:p1",
                    "value": {
                        "session": "local/default",
                        "pane_id": "w1:p1"
                    }
                }
            }
        });

        assert_eq!(
            selected_value(&context, "pane_id").unwrap(),
            ("local/default", "w1:p1")
        );
    }

    #[test]
    fn submit_requires_a_session() {
        let context = json!({
            "step": "agent",
            "selections": {
                "agent": {
                    "id": "w1:p1",
                    "value": { "pane_id": "w1:p1" }
                }
            }
        });

        assert_eq!(
            selected_value(&context, "pane_id").unwrap_err().to_string(),
            "selections.agent.value.session is missing"
        );
    }

    #[test]
    fn stream_applies_deltas_and_clears_once_per_outage() {
        let events = [
            Ok(ServerMessage::Hello {
                protocol: herdr_hub_client::PROTOCOL,
                model: model(Some("local/one"), 1),
            }),
            Ok(ServerMessage::Active {
                version: 2,
                key: Some("local/two".into()),
            }),
            Err(herdr_hub_client::Error::Disconnected),
            Err(herdr_hub_client::Error::Disconnected),
            Ok(ServerMessage::Active {
                version: 3,
                key: Some("ignored/without-hello".into()),
            }),
            Ok(ServerMessage::Hello {
                protocol: herdr_hub_client::PROTOCOL,
                model: model(Some("local/three"), 4),
            }),
        ];
        let mut snapshots = Vec::new();

        consume(events, active_item, |items| {
            snapshots.push(items.iter().map(|item| item.id.clone()).collect::<Vec<_>>());
            Ok(())
        })
        .unwrap();

        assert_eq!(
            snapshots,
            [
                vec!["local/one".to_owned()],
                vec!["local/two".to_owned()],
                Vec::new(),
                vec!["local/three".to_owned()],
            ]
        );
    }

    #[test]
    fn stream_propagates_output_failure() {
        let error = consume(
            [Ok(ServerMessage::Hello {
                protocol: herdr_hub_client::PROTOCOL,
                model: model(Some("local/one"), 1),
            })],
            active_item,
            |_| anyhow::bail!("stdout closed"),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "stdout closed");
    }
}
