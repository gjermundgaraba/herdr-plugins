//! Shared wire types and Herdr plumbing for `herdr-picker` examples.

use std::{
    collections::HashSet,
    fmt,
    io::{self, Write},
    process::ExitCode,
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProviderMessage<T = Vec<Item>> {
    Snapshot(Snapshot<T>),
    Error { error: String },
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
    run_events(|callback| HubClient::new().run(callback), items, emit)
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

fn run_events(
    run: impl FnOnce(&mut dyn FnMut(herdr_hub_client::Result<ServerMessage>) -> bool),
    items: impl Fn(&Model) -> Vec<Item>,
    mut output: impl FnMut(ProviderMessage<&[Item]>) -> Result<()>,
) -> Result<()> {
    let mut state = StreamState::default();
    let mut output_error = None;
    run(
        &mut |event| match state.handle(event, &items, &mut output) {
            Ok(()) => true,
            Err(error) => {
                output_error = Some(error);
                false
            }
        },
    );
    if let Some(error) = output_error {
        return Err(error);
    }
    bail!("hub event stream stopped")
}

#[derive(Default)]
struct StreamState {
    model: Option<Model>,
    previous: Option<Vec<Item>>,
}

impl StreamState {
    fn handle(
        &mut self,
        event: herdr_hub_client::Result<ServerMessage>,
        items: &impl Fn(&Model) -> Vec<Item>,
        output: &mut impl FnMut(ProviderMessage<&[Item]>) -> Result<()>,
    ) -> Result<()> {
        let model = match event {
            Ok(ServerMessage::Hello { model, .. }) => self.model.insert(model),
            Ok(message) => {
                let Some(model) = self.model.as_mut() else {
                    return Ok(());
                };
                HubClient::apply(model, &message);
                model
            }
            Err(error) => {
                self.model = None;
                self.previous = None;
                return output(ProviderMessage::Error {
                    error: format!("Herdr hub unavailable: {error}. Reconnecting…"),
                });
            }
        };
        let next = items(model);
        if self.previous.as_ref() != Some(&next) {
            output(ProviderMessage::Snapshot(Snapshot { items: &next }))?;
            self.previous = Some(next);
        }
        Ok(())
    }
}

fn emit(message: ProviderMessage<&[Item]>) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &message).context("cannot encode provider message")?;
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

    fn consume(
        events: impl IntoIterator<Item = herdr_hub_client::Result<ServerMessage>>,
        items: impl Fn(&Model) -> Vec<Item>,
        output: impl FnMut(ProviderMessage<&[Item]>) -> Result<()>,
    ) -> Result<()> {
        run_events(
            |callback| {
                for event in events {
                    if !callback(event) {
                        break;
                    }
                }
            },
            items,
            output,
        )
    }

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
    fn stream_reports_outage_and_recovers_with_an_empty_snapshot() {
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
            Ok(ServerMessage::Active {
                version: 3,
                key: Some("ignored/without-hello".into()),
            }),
            Ok(ServerMessage::Hello {
                protocol: herdr_hub_client::PROTOCOL,
                model: model(None, 4),
            }),
        ];
        let mut messages = Vec::new();

        consume(events, active_item, |message| {
            messages.push(serde_json::to_value(message)?);
            Ok(())
        })
        .unwrap_err();

        assert_eq!(
            messages,
            [
                json!({ "items": active_item(&model(Some("local/one"), 1)) }),
                json!({ "items": active_item(&model(Some("local/two"), 2)) }),
                json!({ "error": "Herdr hub unavailable: hub disconnected. Reconnecting…" }),
                json!({ "items": [] }),
            ]
        );
    }

    #[test]
    fn unchanged_items_are_suppressed_but_recovery_republishes() {
        let hello = || {
            Ok(ServerMessage::Hello {
                protocol: herdr_hub_client::PROTOCOL,
                model: model(Some("local/one"), 1),
            })
        };
        let mut messages = Vec::new();
        let error = consume(
            [
                hello(),
                Ok(ServerMessage::Active {
                    version: 2,
                    key: Some("local/one".into()),
                }),
                hello(),
                Err(herdr_hub_client::Error::Disconnected),
                hello(),
            ],
            active_item,
            |message| {
                messages.push(serde_json::to_value(message)?);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "hub event stream stopped");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0], messages[2]);
        assert!(messages[1].get("error").is_some());
    }

    #[test]
    fn callback_stops_immediately_and_returns_original_output_error() {
        for outage in [false, true] {
            let mut calls = 0;
            let error = run_events(
                |callback| {
                    let event = if outage {
                        Err(herdr_hub_client::Error::Disconnected)
                    } else {
                        Ok(ServerMessage::Hello {
                            protocol: herdr_hub_client::PROTOCOL,
                            model: model(None, 1),
                        })
                    };
                    assert!(!callback(event));
                },
                active_item,
                |_| {
                    calls += 1;
                    Err(io::Error::new(io::ErrorKind::BrokenPipe, "original output error").into())
                },
            )
            .unwrap_err();
            assert_eq!(calls, 1);
            assert_eq!(
                error.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::BrokenPipe
            );
            assert_eq!(error.to_string(), "original output error");
        }
    }

    #[test]
    fn provider_messages_reject_mixed_or_malformed_frames() {
        for frame in [
            json!({}),
            json!({ "items": [], "error": "offline" }),
            json!({ "error": 42 }),
            json!({ "error": "offline", "unknown": true }),
        ] {
            assert!(serde_json::from_value::<ProviderMessage>(frame).is_err());
        }
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
