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
    let mut previous = None;
    stream_snapshots_from(&client, |snapshot| {
        let next = items(snapshot);
        if previous.as_ref() == Some(&next) {
            return Ok(());
        }
        emit(&next)?;
        previous = Some(next);
        Ok(())
    })
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

fn stream_snapshots_from(
    client: &Client,
    mut publish: impl FnMut(&SessionSnapshot) -> Result<(), String>,
) -> Result<(), String> {
    loop {
        let (snapshot, mut events) = subscribe_events(client)?;
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
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    fn socket_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-picker-herdr-{name}-{}.sock",
            std::process::id()
        ))
    }

    fn accept_request(listener: &UnixListener) -> (UnixStream, Value) {
        let (stream, _) = listener.accept().expect("accept request");
        let mut request = String::new();
        BufReader::new(stream.try_clone().expect("clone stream"))
            .read_line(&mut request)
            .expect("read request");
        (
            stream,
            serde_json::from_str(&request).expect("parse request"),
        )
    }

    fn write_result(stream: &mut UnixStream, request: &Value, result: Value) {
        writeln!(
            stream,
            "{}",
            json!({ "id": request["id"], "result": result })
        )
        .expect("write response");
    }

    fn snapshot_value(panes: Value) -> Value {
        json!({
            "type": "session_snapshot",
            "snapshot": {
                "version": "0.8.0",
                "protocol": 19,
                "workspaces": [],
                "tabs": [],
                "panes": panes,
                "layouts": [],
                "agents": []
            }
        })
    }

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

    #[test]
    fn lifecycle_subscriptions_skip_focus_only_changes() {
        let events = lifecycle_subscriptions()
            .map(|subscription| subscription.kind)
            .collect::<Vec<_>>();

        assert!(events.iter().any(|event| event == "pane.updated"));
        assert!(!events.iter().any(|event| event.ends_with(".focused")));
        assert!(
            !events
                .iter()
                .any(|event| event == "workspace.metadata_updated")
        );
    }

    #[test]
    fn retained_events_stay_on_one_subscription() {
        let path = socket_path("retained-events");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind test socket");
        let server = thread::spawn(move || {
            let empty_snapshot = || snapshot_value(json!([]));

            let (mut stream, request) = accept_request(&listener);
            assert_eq!(request["method"], "session.snapshot");
            write_result(&mut stream, &request, empty_snapshot());

            let (mut events, request) = accept_request(&listener);
            assert_eq!(request["method"], "events.subscribe");
            write_result(
                &mut events,
                &request,
                json!({ "type": "subscription_started" }),
            );
            for pane_id in ["w1:p1", "w1:p2"] {
                writeln!(
                    events,
                    "{}",
                    json!({
                        "event": "pane_updated",
                        "data": { "type": "pane_updated", "pane_id": pane_id }
                    })
                )
                .expect("write event");
            }

            for _ in 0..3 {
                let (mut stream, request) = accept_request(&listener);
                assert_eq!(request["method"], "session.snapshot");
                write_result(&mut stream, &request, empty_snapshot());
            }
            drop(events);

            let (mut stream, request) = accept_request(&listener);
            assert_eq!(request["method"], "session.snapshot");
            writeln!(
                stream,
                "{}",
                json!({
                    "id": request["id"],
                    "error": { "code": "done", "message": "done" }
                })
            )
            .expect("write final error");
        });

        let mut snapshots = 0;
        let error = stream_snapshots_from(&Client::new(&path), |_| {
            snapshots += 1;
            Ok(())
        })
        .expect_err("fake server stops stream");
        assert!(error.contains("done"));
        assert_eq!(snapshots, 3);

        server.join().expect("server thread");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn event_subscription_retries_a_closed_pane() {
        let path = socket_path("closed-pane");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind test socket");
        let server = thread::spawn(move || {
            let (mut stream, request) = accept_request(&listener);
            assert_eq!(request["method"], "session.snapshot");
            write_result(
                &mut stream,
                &request,
                snapshot_value(json!([{
                    "pane_id": "w1:p1",
                    "terminal_id": "terminal-1",
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "focused": false,
                    "agent_status": "idle",
                    "revision": 1
                }])),
            );

            let (mut stream, request) = accept_request(&listener);
            assert_eq!(request["method"], "events.subscribe");
            writeln!(
                stream,
                "{}",
                json!({
                    "id": request["id"],
                    "error": {
                        "code": "pane_not_found",
                        "message": "pane not found"
                    }
                })
            )
            .expect("write error");

            let (mut stream, request) = accept_request(&listener);
            assert_eq!(request["method"], "session.snapshot");
            write_result(&mut stream, &request, snapshot_value(json!([])));

            let (mut stream, request) = accept_request(&listener);
            assert_eq!(request["method"], "events.subscribe");
            write_result(
                &mut stream,
                &request,
                json!({ "type": "subscription_started" }),
            );

            let (mut stream, request) = accept_request(&listener);
            assert_eq!(request["method"], "session.snapshot");
            write_result(&mut stream, &request, snapshot_value(json!([])));
        });

        let (snapshot, subscription) =
            subscribe_events(&Client::new(&path)).expect("retry subscription setup");
        assert!(snapshot.panes.is_empty());
        drop(subscription);

        server.join().expect("server thread");
        let _ = std::fs::remove_file(path);
    }
}
