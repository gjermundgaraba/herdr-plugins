use std::{path::PathBuf, thread};

use herdr_hub_client::{HubClient, ServerMessage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Session {
    pub key: String,
    pub name: String,
    pub socket_path: Option<PathBuf>,
}

pub(crate) type Update = herdr_hub_client::Result<ServerMessage>;

pub(crate) fn spawn_updates(on_update: impl FnMut(Update) + Send + 'static) {
    thread::spawn(move || HubClient::new().run(on_update));
}
