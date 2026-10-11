//! The Herdr fork's canonical client-contract fixtures, copied verbatim from
//! its `tests/fixtures/`. Each one is decoded into this crate's types and
//! encoded again; the result must equal the fixture, so a field the daemon
//! adds, renames, or drops fails here instead of being silently ignored.

use herdr_client::{
    AgentStatus, ClientAgent, ClientChanged, ClientEndpointStatus, ClientInfo, ClientJumpParams,
    ClientJumped, ClientList, ClientReport, EventEnvelope,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).expect(&path)
}

/// Decodes `value` and checks that encoding it again loses nothing.
fn decode<T: DeserializeOwned + Serialize>(value: &Value) -> T {
    let decoded: T = serde_json::from_value(value.clone()).expect("fixture decodes");
    assert_eq!(&serde_json::to_value(&decoded).unwrap(), value);
    decoded
}

/// A response's `result` without its `type` tag, which the typed result
/// structs leave to the method that names them.
fn untagged(result: &Value, expected: &str) -> Value {
    let mut result = result.clone();
    assert_eq!(result["type"], expected);
    result.as_object_mut().unwrap().remove("type");
    result
}

#[test]
fn client_list_fixture_decodes_and_round_trips() {
    let response = fixture("api-client-list-v1.json");
    let list: ClientList = decode(&untagged(&response["result"], "client_list"));
    assert_eq!(list.boot_id, "boot-a");
    assert_eq!(list.clients.len(), 2);
    assert_eq!(
        list.clients[1],
        ClientInfo {
            client_id: 8,
            report: None
        }
    );

    let report = list.clients[0].report.as_ref().expect("client 7 reported");
    assert_eq!(list.clients[0].client_id, 7);
    assert_eq!(report.focused, Some(true));
    assert_eq!(report.active_endpoint_id.as_deref(), Some("local"));
    assert_eq!(
        report
            .endpoints
            .iter()
            .map(|endpoint| (
                endpoint.endpoint_id.as_str(),
                endpoint.status.as_str(),
                endpoint.jump_boot_id()
            ))
            .collect::<Vec<_>>(),
        [
            ("local", ClientEndpointStatus::ONLINE, Some("boot-a")),
            (
                "ssh:0123456789abcdef0123456789abcdef",
                ClientEndpointStatus::ONLINE,
                Some("boot-b")
            ),
            (
                "ssh:fedcba9876543210fedcba9876543210",
                ClientEndpointStatus::RECONNECTING,
                None
            ),
        ]
    );

    // The rows keep the client's display order, across endpoints.
    assert_eq!(
        report
            .agents
            .iter()
            .map(|row| (row.endpoint_id.as_str(), row.agent.pane_id.as_str()))
            .collect::<Vec<_>>(),
        [
            ("local", "w1:p1"),
            ("local", "w2:p1"),
            ("ssh:0123456789abcdef0123456789abcdef", "w1:p4"),
            ("local", "w2:p2"),
        ]
    );
    let focused = &report.agents[1];
    assert!(focused.focused);
    assert_eq!(focused.workspace_label, "web");
    assert_eq!(focused.workspace_number, 2);
    assert_eq!(focused.cwd.as_deref(), Some("/work/web"));
    assert_eq!(focused.agent.agent_status.as_str(), AgentStatus::BLOCKED);
    assert_eq!(focused.agent.priority_key, "1.0.00065d7125600000");
    assert_eq!(focused.agent.tokens, [("task".into(), "review".into())]);
    // The row's own focus is the client's; the record's is its endpoint's.
    assert!(!report.agents[2].focused && report.agents[2].agent.focused);
    assert_eq!(
        report.agents[0].agent.priority_mark.as_deref(),
        Some("high")
    );
}

#[test]
fn client_changed_fixture_names_the_client_only() {
    let events = fixture("api-client-changed-v1.json");
    let changes: Vec<ClientChanged> = events
        .as_array()
        .expect("an array of events")
        .iter()
        .map(|event| {
            let envelope: EventEnvelope = serde_json::from_value(event.clone()).unwrap();
            let changed = ClientChanged::from_event(&envelope)
                .expect("a client_changed event")
                .expect("decodes");
            assert_eq!(
                serde_json::to_value(&changed).unwrap(),
                untagged(&event["data"], "client_changed")
            );
            changed
        })
        .collect();
    assert_eq!(changes, [ClientChanged { client_id: 7 }]);

    let other = EventEnvelope {
        event: "pane_focused".into(),
        data: json!({"type": "pane_focused", "pane_id": "w1:p1"}),
    };
    assert!(ClientChanged::from_event(&other).is_none());
    assert_eq!(ClientChanged::subscription().kind, "client.changed");
}

#[test]
fn client_jump_fixture_is_the_request_this_crate_sends() {
    let exchange = fixture("api-client-jump-v1.json");
    assert_eq!(exchange["request"]["method"], "client.jump");
    let params: ClientJumpParams = decode(&exchange["request"]["params"]);
    assert_eq!(
        params,
        ClientJumpParams {
            client_id: 7,
            endpoint_id: "ssh:0123456789abcdef0123456789abcdef".into(),
            boot_id: "boot-b".into(),
            pane_id: "w1:p4".into(),
        }
    );
    let jumped: ClientJumped = decode(&untagged(&exchange["response"]["result"], "client_jumped"));
    assert_eq!(
        (jumped.client_id, jumped.pane_id.as_str()),
        (params.client_id, params.pane_id.as_str())
    );
    assert_eq!(jumped.endpoint_id, params.endpoint_id);
}

#[test]
fn client_report_fixture_is_the_report_client_list_returns() {
    let request = fixture("endpoint-client-report-v1.json");
    assert_eq!(request["method"], "client.report");
    let report: ClientReport = decode(&request["params"]);
    let list: ClientList =
        serde_json::from_value(fixture("api-client-list-v1.json")["result"].clone()).unwrap();
    assert_eq!(Some(&report), list.clients[0].report.as_ref());
}

#[test]
fn snapshot_fixture_agents_are_the_records_report_rows_carry() {
    let snapshot = fixture("endpoint-snapshot-shared-v1.json");
    let agents: Vec<ClientAgent> = decode(&snapshot["agents"]);
    let report: ClientReport =
        serde_json::from_value(fixture("endpoint-client-report-v1.json")["params"].clone())
            .unwrap();
    let local: Vec<&ClientAgent> = report
        .agents
        .iter()
        .filter(|row| row.endpoint_id == "local")
        .map(|row| &row.agent)
        .collect();
    assert_eq!(agents.iter().collect::<Vec<_>>(), local);

    // Keys order agents as plain strings, smallest first.
    let mut keys: Vec<&str> = agents
        .iter()
        .map(|agent| agent.priority_key.as_str())
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "0.1.00065d7128fb2820",
            "1.0.00065d7125600000",
            "1.3.fff9a28ed378f1ff"
        ]
    );
}

#[test]
fn a_record_without_priority_fields_decodes_an_empty_key_and_no_mark() {
    let mut record = fixture("endpoint-snapshot-shared-v1.json")["agents"][0].clone();
    let fields = record.as_object_mut().unwrap();
    fields.remove("priority_key");
    fields.remove("priority_mark");
    let agent: ClientAgent = serde_json::from_value(record).unwrap();
    assert_eq!(agent.priority_key, "");
    assert_eq!(agent.priority_mark, None);
}
