//! The check every dispatched keystroke and jump passes first.

use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};

use herdr_client::follow::{ClientKey, Directory, FollowedClient};

/// Why captured work was dropped instead of sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the tool is stopping")]
    Stopping,
    #[error("the captured Herdr client is no longer the one the device follows")]
    NotFollowed,
    #[error("the Herdr client's window does not have focus")]
    NotFocused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Followed {
    client: ClientKey,
    focused: bool,
}

impl Followed {
    fn of(client: Option<FollowedClient<'_>>) -> Option<Self> {
        client.map(|client| Self {
            client: client.key(),
            focused: client.has_focus(),
        })
    }
}

#[derive(Debug, Default)]
struct Published {
    /// What the tool published with [`Focus::follow`].
    followed: Option<Followed>,
    /// What the follower saw last, where the tool publishes from a loop of
    /// its own and calls [`Focus::limit`]: `followed` may then come from an
    /// older directory, and holds only as far as this agrees with it.
    /// `None` where the follower's callback publishes itself.
    seen: Option<Option<Followed>>,
}

/// The followed client as the follower last published it, and whether its
/// window had focus then, shared with the threads that dispatch some time
/// after a press.
#[derive(Debug, Clone)]
pub struct Focus {
    published: Arc<Mutex<Published>>,
    stopping: Arc<AtomicBool>,
}

impl Focus {
    /// Nothing is followed yet. Once `stopping` is set, every check fails.
    pub fn new(stopping: Arc<AtomicBool>) -> Self {
        Self {
            published: Arc::default(),
            stopping,
        }
    }

    fn published(&self) -> MutexGuard<'_, Published> {
        self.published
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Publishes [`Directory::followed_client`].
    pub fn follow(&self, client: Option<FollowedClient<'_>>) {
        self.published().followed = Followed::of(client);
    }

    /// Publishes the followed client and whether its window has focus.
    pub fn set(&self, client: Option<ClientKey>, focused: bool) {
        self.published().followed = client.map(|client| Followed { client, focused });
    }

    /// Limits what [`Self::follow`] published to what the follower sees
    /// now. It is for a tool that calls `follow` from a loop of its own,
    /// some time after the follower handed it a directory: call this from
    /// the follower's callback, before passing the directory on. From then
    /// on a check passes only as far as the two agree, so a client that
    /// lost focus or is no longer followed is refused at once, whatever the
    /// loop publishes from an older directory, and one that gained either
    /// is accepted when the loop publishes it.
    pub fn limit(&self, client: Option<FollowedClient<'_>>) {
        self.published().seen = Some(Followed::of(client));
    }

    /// Whether a jump captured for `client` may still be sent: it is still
    /// the followed client. Its window need not have focus.
    pub fn check_followed(&self, client: &ClientKey) -> Result<(), Refusal> {
        self.check(client).map(drop)
    }

    /// Whether keyboard events captured for `client` may still be sent: it
    /// is still the followed client and its window has focus, so it is the
    /// frontmost app.
    pub fn check_focused(&self, client: &ClientKey) -> Result<(), Refusal> {
        if self.check(client)? {
            Ok(())
        } else {
            Err(Refusal::NotFocused)
        }
    }

    /// Whether the followed client `client` has focus.
    fn check(&self, client: &ClientKey) -> Result<bool, Refusal> {
        if self.is_stopping() {
            return Err(Refusal::Stopping);
        }
        let published = self.published();
        let focused = |followed: &Option<Followed>| {
            followed
                .as_ref()
                .filter(|followed| followed.client == *client)
                .map(|followed| followed.focused)
                .ok_or(Refusal::NotFollowed)
        };
        // Both views must name the client before either one's focus counts:
        // an unfocused publication must not hide that the follower no
        // longer follows it.
        let followed = focused(&published.followed)?;
        let seen = match &published.seen {
            Some(seen) => focused(seen)?,
            None => true,
        };
        Ok(followed && seen)
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    /// Refuses all work from now on, including what is already queued: the
    /// follower publishes nothing more, so its last word must not be trusted.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        self.set(None, false);
    }
}

/// The log line for a change of followed client between two directories.
pub fn selection_change(before: &Directory, after: &Directory) -> Option<String> {
    (before.followed != after.followed).then(|| match after.followed_client() {
        Some(client) => format!(
            "following Herdr client {} of session {}",
            client.client_id, client.session
        ),
        None => "following no Herdr client".into(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use herdr_client::{
        ClientEndpointStatus, ClientInfo, ClientReport, ClientReportEndpoint,
        follow::{DaemonClients, DaemonState},
    };

    pub(crate) fn client(session: &str) -> ClientKey {
        ClientKey {
            socket_path: format!("/run/herdr/{session}/herdr.sock").into(),
            boot_id: "daemon-boot".into(),
            client_id: 7,
        }
    }

    /// `client` is followed and its window has focus.
    pub(crate) fn focused_on(client: Option<ClientKey>) -> Focus {
        let focus = Focus::new(Arc::default());
        focus.set(client, true);
        focus
    }

    #[test]
    fn only_the_followed_client_passes_and_nothing_after_stop() {
        let focus = focused_on(Some(client("work")));
        assert_eq!(focus.check_followed(&client("work")), Ok(()));
        assert_eq!(focus.check_focused(&client("work")), Ok(()));
        let mut restarted = client("work");
        restarted.boot_id = "daemon-boot-2".into();
        let mut sibling = client("work");
        sibling.client_id = 8;
        for other in [client("default"), restarted, sibling] {
            for check in [Focus::check_followed, Focus::check_focused] {
                assert_eq!(
                    check(&focus, &other),
                    Err(Refusal::NotFollowed),
                    "{other:?}"
                );
            }
        }
        focus.set(None, false);
        assert_eq!(
            focus.check_followed(&client("work")),
            Err(Refusal::NotFollowed)
        );

        let focus = focused_on(Some(client("work")));
        let clone = focus.clone();
        focus.stop();
        assert!(clone.is_stopping());
        // What is published after the stop changes nothing.
        clone.set(Some(client("work")), true);
        for check in [Focus::check_followed, Focus::check_focused] {
            assert_eq!(check(&clone, &client("work")), Err(Refusal::Stopping));
        }
    }

    #[test]
    fn another_app_in_front_allows_jumps_and_refuses_typing() {
        let focus = Focus::new(Arc::default());
        focus.set(Some(client("work")), false);
        assert_eq!(focus.check_followed(&client("work")), Ok(()));
        assert_eq!(
            focus.check_focused(&client("work")),
            Err(Refusal::NotFocused)
        );
        focus.set(Some(client("work")), true);
        assert_eq!(focus.check_focused(&client("work")), Ok(()));
    }

    fn directory(focused: [Option<bool>; 2]) -> Vec<DaemonClients> {
        vec![DaemonClients {
            session: "work".into(),
            socket_path: "/run/work/herdr.sock".into(),
            state: DaemonState::Following {
                boot_id: "boot".into(),
            },
            clients: [3, 7]
                .into_iter()
                .zip(focused)
                .map(|(client_id, focused)| ClientInfo {
                    client_id,
                    report: Some(ClientReport {
                        focused,
                        active_endpoint_id: None,
                        endpoints: vec![ClientReportEndpoint {
                            endpoint_id: "local".into(),
                            label: "Local".into(),
                            status: ClientEndpointStatus::ONLINE.into(),
                            boot_id: Some("boot".into()),
                        }],
                        agents: Vec::new(),
                    }),
                })
                .collect(),
        }]
    }

    #[test]
    fn the_published_directory_sets_both_checks() {
        let focus = Focus::new(Arc::default());
        let key = ClientKey {
            socket_path: "/run/work/herdr.sock".into(),
            boot_id: "boot".into(),
            client_id: 7,
        };
        let front = Directory::default().updated(directory([Some(false), Some(true)]));
        focus.follow(front.followed_client());
        assert_eq!(focus.check_focused(&key), Ok(()));
        let behind = front.updated(directory([Some(false), Some(false)]));
        focus.follow(behind.followed_client());
        assert_eq!(focus.check_followed(&key), Ok(()));
        assert_eq!(focus.check_focused(&key), Err(Refusal::NotFocused));
        focus.follow(None);
        assert_eq!(focus.check_followed(&key), Err(Refusal::NotFollowed));
    }

    #[test]
    fn a_limit_takes_away_at_once_and_gives_nothing() {
        let key = |client_id| ClientKey {
            socket_path: "/run/work/herdr.sock".into(),
            boot_id: "boot".into(),
            client_id,
        };
        let front = Directory::default().updated(directory([Some(false), Some(true)]));
        let behind = front.updated(directory([Some(false), Some(false)]));
        let other = behind.updated(directory([Some(true), Some(false)]));
        assert_eq!(other.followed, Some(key(3)));

        // The tool's loop published the client with its window in front.
        let focus = Focus::new(Arc::default());
        focus.follow(front.followed_client());
        focus.limit(front.followed_client());
        assert_eq!(focus.check_focused(&key(7)), Ok(()));

        // The follower sees the window lose focus. The loop has not caught
        // up, and publishing its older directory again changes nothing.
        focus.limit(behind.followed_client());
        focus.follow(front.followed_client());
        assert_eq!(focus.check_focused(&key(7)), Err(Refusal::NotFocused));
        assert_eq!(focus.check_followed(&key(7)), Ok(()));

        // Another client takes focus: the one the loop published is no
        // longer followed, and the new one not before the loop says so.
        focus.limit(other.followed_client());
        for client in [key(7), key(3)] {
            assert_eq!(focus.check_followed(&client), Err(Refusal::NotFollowed));
        }
        focus.follow(other.followed_client());
        assert_eq!(focus.check_focused(&key(3)), Ok(()));

        // A gain the follower saw counts once the loop publishes it, and
        // what the loop withholds is not given by the follower.
        focus.follow(behind.followed_client());
        focus.limit(front.followed_client());
        assert_eq!(focus.check_focused(&key(7)), Err(Refusal::NotFocused));
        focus.follow(front.followed_client());
        assert_eq!(focus.check_focused(&key(7)), Ok(()));
        focus.follow(None);
        assert_eq!(focus.check_followed(&key(7)), Err(Refusal::NotFollowed));
        focus.limit(None);
        focus.follow(front.followed_client());
        assert_eq!(focus.check_followed(&key(7)), Err(Refusal::NotFollowed));
    }

    #[test]
    fn a_limit_takes_an_unfocused_client_away_at_once() {
        let key = |client_id| ClientKey {
            socket_path: "/run/work/herdr.sock".into(),
            boot_id: "boot".into(),
            client_id,
        };
        let front = Directory::default().updated(directory([Some(false), Some(true)]));
        let behind = front.updated(directory([Some(false), Some(false)]));
        let other = behind.updated(directory([Some(true), Some(false)]));
        assert_eq!(other.followed, Some(key(3)));
        let gone = behind.updated(Vec::new());

        // The tool's loop published the client with another app in front,
        // or with its focus unknown. Then the follower sees another client
        // take focus, or sees no client at all.
        for published in [Some(false), None] {
            let published = front.updated(directory([Some(false), published]));
            assert_eq!(published.followed, Some(key(7)));
            for newer in [&other, &gone] {
                let focus = Focus::new(Arc::default());
                focus.follow(published.followed_client());
                focus.limit(published.followed_client());
                assert_eq!(focus.check_followed(&key(7)), Ok(()));
                assert_eq!(focus.check_focused(&key(7)), Err(Refusal::NotFocused));

                focus.limit(newer.followed_client());
                for check in [Focus::check_followed, Focus::check_focused] {
                    assert_eq!(
                        check(&focus, &key(7)),
                        Err(Refusal::NotFollowed),
                        "{:?}",
                        newer.followed
                    );
                }
                // Publishing the older directory again brings nothing back.
                focus.follow(published.followed_client());
                assert_eq!(focus.check_followed(&key(7)), Err(Refusal::NotFollowed));
            }
        }
    }

    #[test]
    fn a_change_of_followed_client_is_logged_once() {
        let none = Directory::default();
        // With nothing followed, a reporting client is followed anyway.
        let seventh = none.updated(directory([Some(false), Some(false)]));
        let third = seventh.updated(directory([Some(true), Some(false)]));
        // The window losing focus is not a change of client.
        let third_behind = third.updated(directory([Some(false), Some(false)]));
        // Nor is its daemon failing to answer: the client is kept.
        let mut offline = directory([None, None]);
        offline[0].state = DaemonState::Offline;
        let third_unknown = third_behind.updated(offline);
        assert!(third_unknown.followed_client().is_none());
        assert_eq!(selection_change(&none, &none), None);
        assert_eq!(
            selection_change(&none, &seventh).as_deref(),
            Some("following Herdr client 7 of session work")
        );
        assert_eq!(
            selection_change(&seventh, &third).as_deref(),
            Some("following Herdr client 3 of session work")
        );
        assert_eq!(selection_change(&third, &third_behind), None);
        assert_eq!(selection_change(&third_behind, &third_unknown), None);
        assert_eq!(
            selection_change(&third_unknown, &none).as_deref(),
            Some("following no Herdr client")
        );
    }
}
