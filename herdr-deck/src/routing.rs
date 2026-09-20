use crate::frontends::{ClientRoute, ClientState, Model, Update, project_agent};
use herdr_client::{AgentInfo, WorkspaceInfo};
use herdr_frontend::InputTarget;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentIdentity {
    pub pane_id: String,
    pub agent: Option<String>,
}
impl From<&AgentInfo> for AgentIdentity {
    fn from(agent: &AgentInfo) -> Self {
        Self {
            pane_id: agent.pane_id.clone(),
            agent: agent.agent.clone(),
        }
    }
}
fn focused_client(model: &Model) -> Option<&ClientState> {
    let client = model.foremost_client()?;
    (model
        .clients
        .iter()
        .filter(|c| c.client_id == client.client_id)
        .count()
        == 1)
        .then_some(client)
}
pub fn active_endpoint(model: &Model) -> Option<&str> {
    let client = focused_client(model)?;
    let id = client.active_endpoint_id.as_deref()?;
    client.route(id).map(|_| id)
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct ViewIdentity {
    client_id: String,
    input_ready: bool,
    active_endpoint: Option<String>,
    input_target: Option<InputTarget>,
    agent: Option<AgentIdentity>,
    routes: Vec<ClientRoute>,
}
fn view_identity(model: Option<&Model>) -> Option<ViewIdentity> {
    let client = focused_client(model?)?;
    let mut routes = client
        .endpoints
        .iter()
        .filter_map(|e| client.route(&e.endpoint_id))
        .collect::<Vec<_>>();
    routes.sort_by(|a, b| a.endpoint_id.cmp(&b.endpoint_id));
    Some(ViewIdentity {
        client_id: client.client_id.clone(),
        input_ready: client.input_ready,
        active_endpoint: client.active_endpoint_id.clone(),
        input_target: client.input_target.clone(),
        agent: client.input_target.as_ref().and_then(|input| {
            let snapshot = client
                .endpoints
                .iter()
                .find(|e| e.endpoint_id == input.endpoint_id)?
                .snapshot
                .as_ref()?;
            let agent = snapshot
                .agents
                .iter()
                .find(|a| a.pane_id == input.pane_id)?;
            Some(AgentIdentity {
                pane_id: agent.pane_id.clone(),
                agent: agent.agent.clone(),
            })
        }),
        routes,
    })
}
#[derive(Default, Clone)]
pub struct RoutingState {
    pub model: Option<Model>,
    pub generation: u64,
}
impl RoutingState {
    pub fn update(&mut self, clients: Update) {
        let previous = view_identity(self.model.as_ref());
        self.model = (!clients.is_empty()).then_some(Model { clients });
        if previous != view_identity(self.model.as_ref()) {
            self.generation = self.generation.wrapping_add(1);
        }
    }
    pub fn capture(
        &self,
        allowed: &[String],
        explicit: Option<&str>,
        target: Option<(&str, &AgentInfo)>,
    ) -> Result<ActionRoute, String> {
        let model = self.model.as_ref().ok_or("Herdr frontend is unavailable")?;
        let client = focused_client(model).ok_or("no unique focused local client")?;
        if !client.input_ready {
            return Err("frontend input is unavailable".into());
        }
        let (endpoint_id, target) = if let Some((key, agent)) = target {
            (key, AgentIdentity::from(agent))
        } else {
            let input = client
                .input_target
                .as_ref()
                .ok_or("active client has no selected agent")?;
            let agent = client
                .endpoints
                .iter()
                .find(|e| e.endpoint_id == input.endpoint_id)
                .and_then(|e| e.snapshot.as_ref())
                .and_then(|s| s.agents.iter().find(|a| a.pane_id == input.pane_id))
                .ok_or("active client has no selected agent")?;
            (
                input.endpoint_id.as_str(),
                AgentIdentity {
                    pane_id: agent.pane_id.clone(),
                    agent: agent.agent.clone(),
                },
            )
        };
        if client.active_endpoint_id.as_deref() != Some(endpoint_id) {
            return Err("target endpoint is not active; focus that endpoint first".into());
        }
        if !allowed.is_empty() && !allowed.iter().any(|k| k == endpoint_id) {
            return Err("target endpoint is outside the configured allow-list".into());
        }
        if explicit.is_some_and(|k| k != endpoint_id) {
            return Err("selected endpoint is not active; focus that endpoint first".into());
        }
        let route = ActionRoute {
            session: endpoint_id.into(),
            client_route: client
                .route(endpoint_id)
                .ok_or("target endpoint is unavailable")?,
            source_pane: client.input_target.as_ref().map(|i| i.pane_id.clone()),
            target,
            generation: self.generation,
        };
        self.validate(&route)?;
        Ok(route)
    }
    pub fn validate(&self, route: &ActionRoute) -> Result<(), String> {
        let model = self.model.as_ref().ok_or("Herdr frontend is unavailable")?;
        let client = focused_client(model).ok_or("no unique focused local client")?;
        if self.generation != route.generation {
            return Err("stale Herdr client route".into());
        }
        validate_client(client, route)
    }
}

pub fn validate_client(client: &ClientState, route: &ActionRoute) -> Result<(), String> {
    if client.focused != Some(true)
        || !client.input_ready
        || client.active_endpoint_id.as_deref() != Some(&route.session)
        || client.route(&route.session).as_ref() != Some(&route.client_route)
        || client.input_target.as_ref().map(|i| &i.pane_id) != route.source_pane.as_ref()
    {
        return Err("stale Herdr client route".into());
    }
    let present = client
        .endpoints
        .iter()
        .find(|e| e.endpoint_id == route.session)
        .and_then(|e| e.snapshot.as_ref())
        .is_some_and(|s| {
            s.agents
                .iter()
                .any(|a| a.pane_id == route.target.pane_id && a.agent == route.target.agent)
        });
    if !present {
        return Err("captured agent disappeared or changed identity".into());
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionRoute {
    pub session: String,
    pub client_route: ClientRoute,
    pub source_pane: Option<String>,
    pub target: AgentIdentity,
    pub generation: u64,
}
// Display-only projection. Runtime inventories and Hub routes are not involved.
#[derive(Clone, Debug)]
pub struct DisplayModel {
    pub sessions: Vec<SessionState>,
}
#[derive(Clone, Debug)]
pub struct SessionState {
    pub key: String,
    pub name: String,
    pub connected: bool,
    pub workspaces: Vec<WorkspaceInfo>,
    pub agents: Vec<AgentInfo>,
}
pub fn project_model(raw: &Model) -> DisplayModel {
    let sessions = focused_client(raw)
        .into_iter()
        .flat_map(|client| {
            client
                .endpoints
                .iter()
                .filter(|e| e.is_available())
                .filter_map(|endpoint| {
                    let snapshot = endpoint.snapshot.as_ref()?;
                    Some(SessionState {
                        key: endpoint.endpoint_id.clone(),
                        name: endpoint.label.clone(),
                        connected: true,
                        workspaces: snapshot
                            .workspaces
                            .iter()
                            .map(|w| WorkspaceInfo {
                                workspace_id: w.workspace_id.clone(),
                                number: w.number,
                                label: w.label.clone(),
                                focused: w.focused,
                                pane_count: snapshot
                                    .panes
                                    .iter()
                                    .filter(|p| p.workspace_id == w.workspace_id)
                                    .count(),
                                tab_count: snapshot
                                    .tabs
                                    .iter()
                                    .filter(|t| t.workspace_id == w.workspace_id)
                                    .count(),
                                active_tab_id: w.active_tab_id.clone(),
                                agent_status: w.agent_status.as_str().into(),
                                tokens: Default::default(),
                                worktree: None,
                            })
                            .collect(),
                        agents: snapshot
                            .agents
                            .iter()
                            .map(|a| {
                                let mut agent = project_agent(a, snapshot);
                                agent.focused = client.input_target.as_ref().is_some_and(|i| {
                                    i.endpoint_id == endpoint.endpoint_id && i.pane_id == a.pane_id
                                });
                                agent
                            })
                            .collect(),
                    })
                })
        })
        .collect();
    DisplayModel { sessions }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    pub fn client() -> ClientState {
        ClientState {
            socket_path: "/tmp/test-deck.sock".into(),
            snapshot: serde_json::from_value(json!({
                "client_id":"client-a","pid":1,"revision":1,"focused":true,"input_ready":true,
                "active_endpoint_id":"local","input_target":{"endpoint_id":"local","pane_id":"p1"},
                "endpoints":[{"endpoint_id":"local","label":"Work","status":"online","boot_id":"boot-a",
                    "snapshot":{"boot_id":"boot-a","revision":1,"focused_workspace_id":"w1","focused_tab_id":"t1","focused_pane_id":"p1",
                    "workspaces":[{"workspace_id":"w1","active_tab_id":"t1","number":1,"label":"Work","focused":true,"agent_status":"working"}],"tabs":[],"panes":[],
                    "agents":[{"pane_id":"p1","workspace_id":"w1","tab_id":"t1","agent":"codex","agent_status":"idle","focused":false,"state_change_seq":1,"state_labels":[],"tokens":[]},
                              {"pane_id":"p2","workspace_id":"w1","tab_id":"t1","agent":"claude","agent_status":"done","focused":true,"state_change_seq":2,"state_labels":[],"tokens":[]}]
                }}]
            })).unwrap(),
        }
    }
    #[test]
    fn frontend_inventory_controls_display_without_mutating_raw() {
        let raw = Model {
            clients: vec![client()],
        };
        let projected = project_model(&raw);
        assert_eq!(projected.sessions[0].key, "local");
        assert_eq!(projected.sessions[0].workspaces[0].label, "Work");
        assert!(projected.sessions[0].agents[0].focused);
        assert!(!projected.sessions[0].agents[1].focused);
        assert_eq!(
            projected.sessions[0].agents[1].agent_status.as_str(),
            "done"
        );
        assert!(
            !raw.clients[0].endpoints[0]
                .snapshot
                .as_ref()
                .unwrap()
                .agents[0]
                .focused
        );
    }
    #[test]
    fn ambiguous_focus_missing_boot_and_offline_endpoints_are_not_routable() {
        for case in 0..9 {
            let mut clients = vec![client()];
            match case {
                0 => clients.push(client()),
                1 => clients[0].snapshot.focused = None,
                2 => clients[0].snapshot.focused = Some(false),
                3 => clients[0].snapshot.endpoints[0].boot_id = None,
                4 => clients[0].snapshot.endpoints[0].status = "offline".into(),
                5 => clients[0].snapshot.endpoints[0].snapshot = None,
                6 => clients[0].snapshot.input_ready = false,
                7 => clients[0].snapshot.input_target = None,
                _ => clients.clear(),
            }
            let mut routing = RoutingState::default();
            routing.update(clients);
            assert!(routing.capture(&[], None, None).is_err(), "case {case}");
        }
    }
    #[test]
    fn queued_routes_reject_focus_endpoint_and_source_round_trips_and_eof() {
        for case in 0..7 {
            let initial = client();
            let mut routing = RoutingState::default();
            routing.update(vec![initial.clone()]);
            let route = routing.capture(&[], None, None).unwrap();
            let mut next = initial.clone();
            match case {
                0 => next.snapshot.client_id = "other".into(),
                1 => next.snapshot.focused = Some(false),
                2 => next.snapshot.endpoints[0].boot_id = Some("new-boot".into()),
                3 => next.snapshot.endpoints[0].status = "reconnecting".into(),
                4 => next.snapshot.input_target.as_mut().unwrap().pane_id = "p2".into(),
                5 => next.snapshot.input_ready = false,
                _ => next.socket_path = "/tmp/replaced.sock".into(),
            }
            routing.update(vec![next]);
            assert!(routing.validate(&route).is_err());
            routing.update(vec![initial]);
            assert!(routing.validate(&route).is_err(), "roundtrip {case}");
            let route = routing.capture(&[], None, None).unwrap();
            routing.update(vec![]);
            assert!(routing.validate(&route).is_err());
        }
    }
    #[test]
    fn filters_capture_origin_and_target_agent_identity() {
        let initial = client();
        let mut routing = RoutingState::default();
        routing.update(vec![initial.clone()]);
        assert!(routing.capture(&[], Some("other"), None).is_err());
        assert!(routing.capture(&["other".into()], None, None).is_err());
        let displayed = project_model(routing.model.as_ref().unwrap());
        let target = &displayed.sessions[0].agents[1];
        assert!(
            routing
                .capture(&[], None, Some(("remote", target)))
                .is_err()
        );
        let route = routing.capture(&[], None, Some(("local", target))).unwrap();
        assert_eq!(route.source_pane.as_deref(), Some("p1"));
        assert_eq!(route.target.pane_id, "p2");
        let mut next = initial;
        next.snapshot.endpoints[0].snapshot.as_mut().unwrap().agents[1].agent =
            Some("replacement".into());
        routing.update(vec![next]);
        assert!(routing.validate(&route).is_err());
    }
    #[test]
    fn status_title_and_background_client_updates_do_not_invalidate_routes() {
        let mut initial = client();
        let mut routing = RoutingState::default();
        routing.update(vec![initial.clone()]);
        let route = routing.capture(&[], None, None).unwrap();
        initial.snapshot.revision += 1;
        initial.snapshot.endpoints[0]
            .snapshot
            .as_mut()
            .unwrap()
            .agents[0]
            .title = Some("updated".into());
        initial.snapshot.endpoints[0]
            .snapshot
            .as_mut()
            .unwrap()
            .agents[0]
            .agent_status = "done".into();
        let mut other = client();
        other.snapshot.client_id = "background".into();
        other.snapshot.focused = Some(false);
        routing.update(vec![initial, other]);
        routing.validate(&route).unwrap();
        assert_eq!(
            project_model(routing.model.as_ref().unwrap()).sessions[0].agents[0]
                .agent_status
                .as_str(),
            "done"
        );
    }
}
