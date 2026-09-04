use herdr_hub_client::{HostState, Model, ServerMessage, SessionState};

#[derive(Debug)]
pub(crate) struct Store {
    model: Model,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            model: Model {
                version: 0,
                active: None,
                hosts: vec![HostState {
                    key: "local".into(),
                    connected: true,
                    error: None,
                }],
                sessions: Vec::new(),
            },
        }
    }
}

impl Store {
    pub(crate) fn get(&self) -> &Model {
        &self.model
    }

    pub(crate) fn session(&self, key: &str) -> Option<&SessionState> {
        self.model
            .sessions
            .iter()
            .find(|session| session.key == key)
    }

    pub(crate) fn publish_session(&mut self, session: SessionState) -> Option<ServerMessage> {
        if let Some(current) = self
            .model
            .sessions
            .iter_mut()
            .find(|current| current.key == session.key)
        {
            if *current == session {
                return None;
            }
            *current = session.clone();
        } else {
            self.model.sessions.push(session.clone());
            self.model
                .sessions
                .sort_by(|left, right| left.key.cmp(&right.key));
        }
        self.bump();
        Some(ServerMessage::Session {
            version: self.model.version,
            session,
        })
    }

    pub(crate) fn remove_session(&mut self, key: &str) -> Option<ServerMessage> {
        let index = self
            .model
            .sessions
            .iter()
            .position(|session| session.key == key)?;
        self.model.sessions.remove(index);
        self.bump();
        Some(ServerMessage::SessionRemoved {
            version: self.model.version,
            key: key.into(),
        })
    }

    pub(crate) fn remove_host_sessions(&mut self, host: &str) -> Vec<ServerMessage> {
        let keys = self
            .model
            .sessions
            .iter()
            .filter(|session| session.host == host)
            .map(|session| session.key.clone())
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| self.remove_session(&key))
            .collect()
    }

    pub(crate) fn set_active(&mut self, key: Option<String>) -> Option<ServerMessage> {
        if self.model.active == key {
            return None;
        }
        self.model.active.clone_from(&key);
        self.bump();
        Some(ServerMessage::Active {
            version: self.model.version,
            key,
        })
    }

    pub(crate) fn set_host(&mut self, host: HostState) -> Option<ServerMessage> {
        if let Some(current) = self
            .model
            .hosts
            .iter_mut()
            .find(|current| current.key == host.key)
        {
            if *current == host {
                return None;
            }
            *current = host.clone();
        } else {
            self.model.hosts.push(host.clone());
            self.model
                .hosts
                .sort_by(|left, right| left.key.cmp(&right.key));
        }
        self.bump();
        Some(ServerMessage::Host {
            version: self.model.version,
            host,
        })
    }

    fn bump(&mut self) {
        self.model.version = self
            .model
            .version
            .checked_add(1)
            .expect("hub model version overflowed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(key: &str, connected: bool) -> SessionState {
        SessionState {
            key: key.into(),
            host: "local".into(),
            name: key.rsplit('/').next().unwrap().into(),
            connected,
            error: None,
            protocol: 20,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            agents: Vec::new(),
            socket_path: None,
            client_focused: None,
        }
    }

    #[test]
    fn updates_are_versioned_and_replace_by_key() {
        let mut store = Store::default();
        assert_eq!(store.get().version, 0);
        store.publish_session(session("local/work", false));
        let message = store.publish_session(session("local/work", true)).unwrap();
        assert_eq!(store.get().sessions.len(), 1);
        assert!(store.get().sessions[0].connected);
        assert!(matches!(message, ServerMessage::Session { version: 2, .. }));
        assert!(store.publish_session(session("local/work", true)).is_none());
        let removed = store.remove_session("local/work").unwrap();
        assert!(matches!(
            removed,
            ServerMessage::SessionRemoved { version: 3, .. }
        ));
        assert!(store.remove_session("local/missing").is_none());
    }

    #[test]
    fn unchanged_singletons_do_not_consume_versions() {
        let mut store = Store::default();
        assert!(store.set_active(None).is_none());
        assert!(
            store
                .set_host(HostState {
                    key: "local".into(),
                    connected: true,
                    error: None,
                })
                .is_none()
        );
        assert_eq!(store.get().version, 0);
        assert!(store.set_active(Some("local/default".into())).is_some());
        assert_eq!(store.get().version, 1);
    }

    #[test]
    fn removes_only_one_hosts_sessions() {
        let mut store = Store::default();
        store.publish_session(session("local/default", true));
        let mut remote = session("workbox/default", true);
        remote.host = "workbox".into();
        store.publish_session(remote);

        let messages = store.remove_host_sessions("workbox");
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            messages[0],
            ServerMessage::SessionRemoved { ref key, .. } if key == "workbox/default"
        ));
        assert!(store.session("local/default").is_some());
        assert!(store.session("workbox/default").is_none());
    }
}
