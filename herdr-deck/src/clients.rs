//! The dashboard's view of the followed Herdr client: the endpoints it is
//! attached to and its Agents list, in the order it reports them.

use herdr_client::{ClientReportAgent, follow::Directory};

/// What the keys and the touch strip draw from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayModel {
    pub sessions: Vec<SessionState>,
    /// The client's Agents list, in its display order. The agents of an
    /// endpoint that is not `connected` are in it too: the client still
    /// reports them, so they keep their keys, shown as unavailable.
    pub agents: Vec<ClientReportAgent>,
}

/// One endpoint of the followed client. `key` is the client's endpoint id,
/// which is what `herdr.sessions` lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionState {
    pub key: String,
    pub name: String,
    /// Online with a known boot: its agents can be jumped to.
    pub connected: bool,
}

/// `None` while no client is attached to any followed daemon, which the
/// dashboard shows as offline. With clients but no followed one to show
/// (none has sent a report, or the followed client's daemon stopped
/// answering) the model is empty: nothing is drawn and nothing can be
/// dispatched. Another app in front changes nothing here.
pub fn project_model(directory: &Directory) -> Option<DisplayModel> {
    if directory.client_count() == 0 {
        return None;
    }
    let Some(client) = directory.followed_client() else {
        return Some(DisplayModel {
            sessions: Vec::new(),
            agents: Vec::new(),
        });
    };
    let sessions = client
        .report
        .endpoints
        .iter()
        .map(|endpoint| SessionState {
            key: endpoint.endpoint_id.clone(),
            name: endpoint.label.clone(),
            connected: endpoint.jump_boot_id().is_some(),
        })
        .collect();
    let agents = client.report.agents.clone();
    Some(DisplayModel { sessions, agents })
}

/// The endpoint the followed client presents, online or not.
pub fn active_endpoint(directory: &Directory) -> Option<&str> {
    directory
        .followed_client()?
        .report
        .active_endpoint_id
        .as_deref()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use herdr_client::{
        ClientEndpointStatus, ClientInfo, ClientList, ClientReport,
        follow::{DaemonClients, DaemonState},
    };

    /// The report of the fork's canonical `client.list` response.
    pub fn fixture_report() -> ClientReport {
        let response: serde_json::Value = serde_json::from_str(include_str!(
            "../../sdk/rust/tests/fixtures/api-client-list-v1.json"
        ))
        .unwrap();
        let list: ClientList = serde_json::from_value(response["result"].clone()).unwrap();
        list.clients[0].report.clone().unwrap()
    }

    pub fn client(client_id: u64, focused: Option<bool>) -> ClientInfo {
        let mut report = fixture_report();
        report.focused = focused;
        ClientInfo {
            client_id,
            report: Some(report),
        }
    }

    pub fn daemon(session: &str, clients: Vec<ClientInfo>) -> DaemonClients {
        DaemonClients {
            session: session.into(),
            socket_path: format!("/run/herdr/{session}/herdr.sock").into(),
            state: DaemonState::Following {
                boot_id: format!("daemon-boot-{session}"),
            },
            clients,
        }
    }

    pub fn directory(daemons: Vec<DaemonClients>) -> Directory {
        Directory::default().updated(daemons)
    }

    #[test]
    fn no_client_is_offline_and_no_followed_client_is_blank() {
        for daemons in [vec![], vec![daemon("default", vec![])]] {
            assert_eq!(project_model(&directory(daemons)), None);
        }
        let silent = ClientInfo {
            client_id: 9,
            report: None,
        };
        let blank = |directory: &Directory| {
            let model = project_model(directory).expect("a client is attached");
            assert!(model.sessions.is_empty() && model.agents.is_empty());
            assert_eq!(active_endpoint(directory), None);
        };
        // No client has sent a report yet.
        blank(&directory(vec![daemon("default", vec![silent.clone()])]));
        // The followed client detached and none of the others reports.
        let followed = directory(vec![daemon("default", vec![client(7, Some(true))])]);
        blank(&followed.updated(vec![daemon("default", vec![silent.clone()])]));
        // Its daemon stopped answering: it is kept, but there is nothing to
        // show of it, and no other client is shown in its place.
        let mut offline = daemon("default", vec![]);
        offline.state = DaemonState::Offline;
        let down = followed.updated(vec![
            offline.clone(),
            daemon("work", vec![client(2, Some(false))]),
        ]);
        blank(&down);
        assert_eq!(project_model(&followed.updated(vec![offline])), None);
        assert_eq!(project_model(&followed.updated(vec![])), None);
    }

    #[test]
    fn a_client_that_never_reported_focus_is_shown_when_no_other_took_it() {
        // A TUI reports no focus until its terminal sends a focus change.
        for focused in [None, Some(false)] {
            let directory = directory(vec![daemon("default", vec![client(7, focused)])]);
            let model = project_model(&directory).expect("a client is attached");
            assert!(!model.agents.is_empty());
            assert_eq!(active_endpoint(&directory), Some("local"));
            assert!(!directory.followed_client().unwrap().has_focus());
        }
    }

    #[test]
    fn another_app_in_front_keeps_the_display_and_the_latest_focus_wins() {
        let followed = directory(vec![daemon("default", vec![client(7, Some(true))])]);
        let model = project_model(&followed);
        assert!(model.as_ref().is_some_and(|model| !model.agents.is_empty()));
        for focused in [Some(false), None] {
            let behind = followed.updated(vec![daemon("default", vec![client(7, focused)])]);
            assert_eq!(project_model(&behind), model);
            assert_eq!(active_endpoint(&behind), Some("local"));
        }

        // A client of another daemon takes focus while the first still
        // reports it: the display moves to the newer one.
        let mut other = client(2, Some(true));
        other.report.as_mut().unwrap().agents.truncate(1);
        let moved = followed.updated(vec![
            daemon("default", vec![client(7, Some(true))]),
            daemon("work", vec![other]),
        ]);
        assert_eq!(project_model(&moved).unwrap().agents.len(), 1);
        assert_eq!(moved.followed_client().unwrap().session, "work");
    }

    #[test]
    fn the_followed_client_supplies_endpoints_and_agents_in_its_order() {
        // The followed client is attached to another daemon than the first.
        let directory = directory(vec![
            daemon("default", vec![client(3, Some(false))]),
            daemon("work", vec![client(7, Some(true))]),
        ]);
        let model = project_model(&directory).unwrap();
        assert_eq!(
            model
                .sessions
                .iter()
                .map(|session| (
                    session.key.as_str(),
                    session.name.as_str(),
                    session.connected
                ))
                .collect::<Vec<_>>(),
            [
                ("local", "Local", true),
                ("ssh:0123456789abcdef0123456789abcdef", "Build", true),
                ("ssh:fedcba9876543210fedcba9876543210", "Staging", false),
            ]
        );
        assert_eq!(model.agents, fixture_report().agents);
        assert_eq!(active_endpoint(&directory), Some("local"));
    }

    #[test]
    fn agents_of_an_endpoint_that_is_not_online_stay_in_the_model() {
        let mut focused = client(7, Some(true));
        let report = focused.report.as_mut().unwrap();
        report.endpoints[0].status = ClientEndpointStatus::RECONNECTING.into();
        let directory = directory(vec![daemon("default", vec![focused])]);
        let model = project_model(&directory).unwrap();
        // The client still reports them and still presents `local`; only
        // the endpoint says that they cannot be reached.
        assert_eq!(model.agents, fixture_report().agents);
        assert_eq!(
            model
                .sessions
                .iter()
                .map(|session| session.connected)
                .collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(active_endpoint(&directory), Some("local"));
    }
}
