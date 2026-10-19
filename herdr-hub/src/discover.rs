//! Local session discovery. The listing and its path validation live in
//! `herdr_client::sessions`; Hub adds the key its inventory files a local
//! session under.

pub(crate) use herdr_client::sessions::LocalSession;

pub(crate) fn session_key(session: &LocalSession) -> String {
    format!("local/{}", session.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_sessions_are_keyed_under_the_local_host() {
        let session = LocalSession {
            name: "work".into(),
            socket_path: "/xdg/herdr/sessions/work/herdr.sock".into(),
        };
        assert_eq!(session_key(&session), "local/work");
    }
}
