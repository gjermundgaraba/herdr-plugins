#[cfg(unix)]
#[test]
fn parses_actual_tui_protocol_seven_projection() {
    let snapshot: herdr_client::frontend::Snapshot =
        serde_json::from_str(include_str!("fixtures/frontend-v7.json")).unwrap();
    assert_eq!(snapshot.client_id, "fixture-client");
    let endpoint = &snapshot.endpoints[0];
    assert!(endpoint.is_available());
    assert!(!endpoint.snapshot.as_ref().unwrap().panes.is_empty());
}
