//! Multi-daemon following, the follow rule across failed watches, and
//! jumps, against in-process mock daemons.
#![cfg(unix)]

mod support;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use herdr_client::{
    ApiError, Client, ClientEndpointStatus, ClientInfo, ClientJumpError, ClientJumped, ClientList,
    ClientReport, ClientReportEndpoint, Error,
    follow::{self, ClientKey, DaemonState, Directory, JumpTarget, Timing},
    sessions::LocalSession,
};
use serde_json::json;
use support::MockDaemon;

const WAIT: Duration = Duration::from_secs(10);

fn timing() -> Timing {
    Timing {
        rescan: Duration::from_millis(30),
        io_timeout: Duration::from_secs(2),
        poll: Duration::from_millis(10),
        reconnect_min: Duration::from_millis(10),
        reconnect_max: Duration::from_millis(40),
    }
}

fn client(client_id: u64, focused: Option<bool>) -> ClientInfo {
    ClientInfo {
        client_id,
        report: Some(ClientReport {
            focused,
            active_endpoint_id: Some("local".into()),
            endpoints: vec![ClientReportEndpoint {
                endpoint_id: "local".into(),
                label: "Local".into(),
                status: ClientEndpointStatus::ONLINE.into(),
                boot_id: Some("boot".into()),
            }],
            agents: Vec::new(),
        }),
    }
}

struct Follower {
    updates: mpsc::Receiver<Directory>,
    latest: Directory,
    sessions: Arc<Mutex<Result<Vec<LocalSession>, String>>>,
    /// How often session discovery has been started.
    discoveries: Arc<AtomicUsize>,
    logs: Arc<Mutex<Vec<String>>>,
    stopping: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Follower {
    /// Follows `sessions`, which a test may change between rescans.
    fn start(sessions: Vec<LocalSession>) -> Self {
        let sessions = Arc::new(Mutex::new(Ok(sessions)));
        let mut follower = Self::discovering({
            let sessions = Arc::clone(&sessions);
            move || sessions.lock().unwrap().clone()
        });
        follower.sessions = sessions;
        follower
    }

    fn discovering(
        mut discover: impl FnMut() -> Result<Vec<LocalSession>, String> + Send + 'static,
    ) -> Self {
        let (tx, updates) = mpsc::channel();
        let discoveries = Arc::new(AtomicUsize::new(0));
        let logs = Arc::new(Mutex::new(Vec::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = follow::spawn_with(
            timing(),
            {
                let discoveries = Arc::clone(&discoveries);
                move || {
                    discoveries.fetch_add(1, Ordering::SeqCst);
                    discover()
                }
            },
            move |directory| tx.send(directory).is_ok(),
            {
                let logs = Arc::clone(&logs);
                move |line| logs.lock().unwrap().push(line)
            },
            Arc::clone(&stopping),
        );
        Self {
            updates,
            latest: Directory::default(),
            sessions: Arc::new(Mutex::new(Ok(Vec::new()))),
            discoveries,
            logs,
            stopping,
            thread: Some(thread),
        }
    }

    /// Waits until the published directory satisfies `done`.
    fn wait(&mut self, what: &str, done: impl Fn(&Directory) -> bool) -> &Directory {
        let deadline = Instant::now() + WAIT;
        while !done(&self.latest) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.updates.recv_timeout(left) {
                Ok(directory) => self.latest = directory,
                Err(_) => panic!("timed out waiting for {what}: {:#?}", self.latest),
            }
        }
        &self.latest
    }

    fn wait_followed(&mut self, expected: Option<(&str, u64)>) {
        self.wait(&format!("followed client {expected:?}"), |directory| {
            directory
                .followed_client()
                .map(|client| (client.session, client.client_id))
                == expected
        });
    }

    /// Waits until `expected` is followed and its window focus is `focused`.
    fn wait_followed_with_focus(&mut self, expected: (&str, u64), focused: bool) {
        self.wait(
            &format!("followed client {expected:?}, focused: {focused}"),
            |directory| {
                directory
                    .followed_client()
                    .map(|client| (client.session, client.client_id, client.has_focus()))
                    == Some((expected.0, expected.1, focused))
            },
        );
    }

    fn logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }

    /// How many logged lines contain `text`.
    fn logged(&self, text: &str) -> usize {
        let logs = self.logs.lock().unwrap();
        logs.iter().filter(|line| line.contains(text)).count()
    }

    /// Waits until a logged line contains `text`.
    fn wait_logged(&self, text: &str) {
        wait_until(&format!("a log line with {text:?}"), || {
            self.logged(text) > 0
        });
    }

    /// Waits until session discovery has been started `count` more times,
    /// so that every discovery but the last of them has also finished.
    fn wait_discoveries(&self, count: usize) {
        let target = self.discoveries.load(Ordering::SeqCst) + count;
        wait_until(&format!("{count} more session discoveries"), || {
            self.discoveries.load(Ordering::SeqCst) >= target
        });
    }
}

/// Polls `done` until it holds, for at most [`WAIT`].
fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl Drop for Follower {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("follower thread");
        }
    }
}

fn following(directory: &Directory, session: &str) -> bool {
    directory.daemons.iter().any(|daemon| {
        daemon.session == session && matches!(daemon.state, DaemonState::Following { .. })
    })
}

/// Waits until `daemon` has received `count` requests for `method`.
fn wait_requests(daemon: &MockDaemon, method: &str, count: usize) {
    wait_until(
        &format!("{count} {method} requests to {}", daemon.name),
        || daemon.requests(method).len() >= count,
    );
}

#[test]
fn the_last_focused_client_is_followed_across_daemons_until_it_detaches() {
    let default = MockDaemon::start("default");
    let work = MockDaemon::start("work");
    default.set_client(ClientInfo {
        client_id: 2,
        report: None,
    });
    let mut follower = Follower::start(vec![default.session(), work.session()]);

    // No client has sent a report yet.
    let directory = follower.wait("both daemons", |directory| {
        following(directory, "default") && following(directory, "work")
    });
    assert_eq!(directory.client_count(), 1);
    assert!(directory.followed_client().is_none());
    assert_eq!(directory.followed, None);
    let mut boots: Vec<_> = directory
        .daemons
        .iter()
        .map(|daemon| (daemon.session.as_str(), &daemon.state))
        .collect();
    boots.sort_by_key(|(session, _)| *session);
    assert_eq!(
        boots,
        [
            (
                "default",
                &DaemonState::Following {
                    boot_id: "boot-default".into()
                }
            ),
            (
                "work",
                &DaemonState::Following {
                    boot_id: "boot-work".into()
                }
            ),
        ]
    );

    // A client whose focus is unknown reports: with nothing followed, it is
    // followed anyway, without focus.
    default.set_client(client(3, None));
    follower.wait_followed_with_focus(("default", 3), false);

    // It takes focus.
    default.set_client(client(3, Some(true)));
    follower.wait_followed_with_focus(("default", 3), true);

    // Another app comes to the front: the client stays followed.
    default.set_client(client(3, Some(false)));
    follower.wait_followed_with_focus(("default", 3), false);
    default.set_client(client(3, Some(true)));
    follower.wait_followed_with_focus(("default", 3), true);

    // A client of another daemon also claims focus: the latest change wins,
    // and it stays followed once the first client gives focus up.
    work.set_client(client(1, Some(true)));
    follower.wait_followed(Some(("work", 1)));
    assert_eq!(follower.latest.client_count(), 3);
    default.set_client(client(3, Some(false)));
    follower.wait("the first client's loss", |directory| {
        directory.daemons.iter().all(|daemon| {
            daemon.session != "default" || daemon.clients.contains(&client(3, Some(false)))
        })
    });
    follower.wait_followed_with_focus(("work", 1), true);
    let key = follower.latest.followed_client().unwrap().key();
    assert_eq!(
        key,
        ClientKey {
            socket_path: work.socket_path.clone(),
            boot_id: "boot-work".into(),
            client_id: 1,
        }
    );
    assert_eq!(follower.latest.followed, Some(key));

    // A client that attaches without taking focus does not replace it.
    default.set_client(client(4, None));
    follower.wait("four clients", |directory| directory.client_count() == 4);
    follower.wait_followed(Some(("work", 1)));

    // The followed client detaches: of the reporting clients left, the one
    // whose focus is unknown is followed before the one without focus.
    work.remove_client(1);
    follower.wait_followed_with_focus(("default", 4), false);
    assert_eq!(follower.latest.client_count(), 3);
    default.set_client(client(3, Some(true)));
    follower.wait_followed(Some(("default", 3)));

    // One subscription per daemon, and a listing only after it started and
    // after changes: a handful each here. Nothing polls.
    std::thread::sleep(Duration::from_millis(150));
    for daemon in [&default, &work] {
        assert_eq!(daemon.requests("events.subscribe").len(), 1);
        let lists = daemon.requests("client.list").len();
        assert!((2..=9).contains(&lists), "{lists} listings");
    }
}

#[test]
fn a_burst_of_changes_costs_one_listing() {
    let daemon = MockDaemon::start("default");
    let mut follower = Follower::start(vec![daemon.session()]);
    follower.wait("the daemon", |directory| following(directory, "default"));
    let before = daemon.requests("client.list").len();

    daemon.set_clients((1..=50).map(|id| client(id, Some(id == 50))).collect());
    follower.wait("fifty clients", |directory| directory.client_count() == 50);
    follower.wait_followed(Some(("default", 50)));
    let lists = daemon.requests("client.list").len() - before;
    // One read normally takes the whole burst; a split read costs one more.
    assert!((1..=3).contains(&lists), "{lists} listings for one burst");
}

#[test]
fn a_hung_discovery_delays_neither_published_changes_nor_stopping() {
    let daemon = MockDaemon::start("default");
    daemon.set_client(client(1, Some(true)));
    let sessions = vec![daemon.session()];
    let (entered, wait_entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let mut calls = 0;
    let mut follower = Follower::discovering(move || {
        calls += 1;
        if calls > 1 {
            // Every later discovery hangs until the test lets it go.
            let _ = entered.send(());
            let _ = released.recv_timeout(WAIT);
        }
        Ok(sessions.clone())
    });
    follower.wait_followed(Some(("default", 1)));
    wait_entered
        .recv_timeout(WAIT)
        .expect("a second discovery started");

    // Focus loss and gain still reach the tool while discovery hangs.
    daemon.set_client(client(1, Some(false)));
    follower.wait_followed_with_focus(("default", 1), false);
    daemon.set_client(client(2, Some(true)));
    follower.wait_followed(Some(("default", 2)));

    // Stopping does not wait for it either.
    let started = Instant::now();
    drop(follower);
    assert!(started.elapsed() < Duration::from_secs(2));
    let _ = release.send(());
}

#[test]
fn a_daemon_that_rejects_the_client_methods_is_logged_once_in_its_own_words() {
    let rejecting = MockDaemon::start("default");
    rejecting.fail_listing(Some(json!({
        "code": "invalid_request",
        "message": "invalid request: unknown variant `client.list`",
    })));
    let current = MockDaemon::start("work");
    current.set_client(client(1, Some(true)));
    let mut follower = Follower::start(vec![rejecting.session(), current.session()]);

    // The other daemon supplies the followed client throughout.
    follower.wait_followed(Some(("work", 1)));
    // It is asked again, with a backoff, and stays offline.
    wait_requests(&rejecting, "client.list", 4);
    let directory = follower.wait("the daemon listed", |directory| {
        directory.daemons.len() == 2
    });
    let rejected = directory
        .daemons
        .iter()
        .find(|daemon| daemon.session == "default")
        .unwrap();
    assert_eq!(rejected.state, DaemonState::Offline);
    assert!(rejected.clients.is_empty());
    let reason = format!(
        "Herdr session default ({}): cannot follow its clients: \
         invalid_request: invalid request: unknown variant `client.list`",
        rejecting.socket_path.display()
    );
    let logs = follower.logs();
    assert_eq!(
        logs.iter()
            .filter(|line| line.contains("default"))
            .collect::<Vec<_>>(),
        [&reason],
        "{logs:?}"
    );

    // Once it answers, it is followed like any other.
    rejecting.fail_listing(None);
    follower.wait("the daemon followed", |directory| {
        following(directory, "default")
    });
    follower.wait_followed(Some(("work", 1)));
}

#[test]
fn a_failed_watch_keeps_the_followed_client_until_a_listing_lacks_it() {
    let daemon = MockDaemon::start("default");
    // Client 5 is the one a follower with nothing followed would pick.
    daemon.set_clients(vec![client(1, Some(true)), client(5, None)]);
    let mut follower = Follower::start(vec![daemon.session()]);
    follower.wait_followed_with_focus(("default", 1), true);
    daemon.set_client(client(1, Some(false)));
    follower.wait_followed_with_focus(("default", 1), false);
    let key = follower.latest.followed.clone().unwrap();

    // A listing fails: the daemon is offline to the follower, which keeps
    // the client without supplying it.
    daemon.fail_listing(Some(json!({"code": "internal", "message": "busy"})));
    daemon.set_client(client(5, None));
    let directory = follower.wait("the daemon offline", |directory| {
        !following(directory, "default")
    });
    assert!(directory.followed_client().is_none());
    assert_eq!(directory.followed, Some(key.clone()));
    assert_eq!(directory.client_count(), 0);
    // It stays that way over more failed attempts.
    let attempts = daemon.requests("client.list").len();
    wait_requests(&daemon, "client.list", attempts + 3);

    // The daemon answers again: the same client is followed, not client 5.
    daemon.fail_listing(None);
    follower.wait_followed_with_focus(("default", 1), false);
    assert!(follower.logs().iter().any(|line| {
        line.contains("Herdr session default: lost the daemon connection: internal: busy")
    }));

    // It detaches during the next failure: the first listing afterwards
    // does not contain it, so it is forgotten and client 5 is followed.
    daemon.fail_listing(Some(json!({"code": "internal", "message": "busy"})));
    daemon.remove_client(1);
    let directory = follower.wait("the daemon offline again", |directory| {
        !following(directory, "default")
    });
    assert_eq!(directory.followed, Some(key));
    daemon.fail_listing(None);
    follower.wait_followed_with_focus(("default", 5), false);
}

#[test]
fn sessions_come_and_go_and_a_lost_daemon_loses_its_clients() {
    let default = MockDaemon::start("default");
    default.set_client(client(1, Some(true)));
    let mut follower = Follower::start(vec![default.session()]);
    follower.wait_followed(Some(("default", 1)));

    // A session started later is picked up by rediscovery.
    let work = MockDaemon::start("work");
    work.set_client(client(5, Some(false)));
    *follower.sessions.lock().unwrap() = Ok(vec![default.session(), work.session()]);
    follower.wait("the new session", |directory| following(directory, "work"));
    follower.wait_followed(Some(("default", 1)));

    // A failing discovery keeps what is already followed. It is logged the
    // first time, and not again while it goes on failing the same way.
    let failure = "session discovery failed: herdr is missing";
    *follower.sessions.lock().unwrap() = Err("herdr is missing".into());
    follower.wait_logged(failure);
    follower.wait_discoveries(3);
    work.set_client(client(5, Some(true)));
    follower.wait_followed(Some(("work", 5)));
    assert_eq!(follower.latest.client_count(), 2);
    assert_eq!(follower.logged(failure), 1);

    // A daemon dies: its clients are gone at once. The followed client is
    // attached to the other one, and stays.
    let default_session = default.session();
    drop(default);
    follower.wait("the default daemon offline", |directory| {
        directory.daemons.iter().any(|daemon| {
            daemon.session == "default"
                && daemon.state == DaemonState::Offline
                && daemon.clients.is_empty()
        })
    });
    follower.wait_followed(Some(("work", 5)));
    assert!(
        follower
            .logs()
            .iter()
            .any(|line| line.contains("Herdr session default: lost the daemon connection"))
    );

    // A session that is no longer listed is dropped entirely, and the
    // followed client with it.
    *follower.sessions.lock().unwrap() = Ok(vec![default_session]);
    follower.wait("only the default session", |directory| {
        directory.daemons.len() == 1 && directory.daemons[0].session == "default"
    });
    assert!(follower.latest.followed_client().is_none());
    assert_eq!(follower.latest.followed, None);
}

#[test]
fn an_undecodable_change_resubscribes_and_keeps_the_followed_client() {
    let daemon = MockDaemon::start("default");
    // Client 5 is the one a follower with nothing followed would pick.
    daemon.set_clients(vec![client(1, Some(true)), client(5, None)]);
    let mut follower = Follower::start(vec![daemon.session()]);
    follower.wait_followed(Some(("default", 1)));
    daemon.set_client(client(1, Some(false)));
    follower.wait_followed_with_focus(("default", 1), false);
    let lists = daemon.requests("client.list").len();
    assert_eq!(daemon.requests("events.subscribe").len(), 1);

    // An event of another kind is passed over; the change that cannot be
    // decoded ends the subscription, and the worker subscribes once more.
    daemon.push_event(&json!({"event": "pane_focused", "data": {"type": "pane_focused"}}));
    daemon.push_event(&json!({"event": "client_changed", "data": {"client_id": "seven"}}));
    follower.wait("the daemon offline", |directory| {
        !following(directory, "default")
    });
    wait_requests(&daemon, "client.list", lists + 1);
    follower.wait_followed_with_focus(("default", 1), false);
    assert_eq!(daemon.requests("events.subscribe").len(), 2);
}

/// Writes the jump and waits for its answer.
fn send(target: &JumpTarget) -> Result<ClientJumped, ClientJumpError> {
    target.start()?.wait()
}

fn jump_target(daemon: &MockDaemon) -> JumpTarget {
    JumpTarget {
        client: ClientKey {
            socket_path: daemon.socket_path.clone(),
            boot_id: "boot-default".into(),
            client_id: 7,
        },
        endpoint_id: "ssh:0123456789abcdef0123456789abcdef".into(),
        boot_id: "boot-b".into(),
        pane_id: "w1:p4".into(),
    }
}

#[test]
fn a_jump_sends_the_contract_request_once_and_returns_the_focused_pane() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/api-client-jump-v1.json")).unwrap();
    let daemon = MockDaemon::start("default");
    daemon.set_jump_reply(json!({"result": fixture["response"]["result"]}));

    let jumped = send(&jump_target(&daemon)).expect("the jump succeeds");
    assert_eq!(
        jumped,
        ClientJumped {
            client_id: 7,
            endpoint_id: "ssh:0123456789abcdef0123456789abcdef".into(),
            pane_id: "w1:p4".into(),
        }
    );
    let requests = daemon.requests("client.jump");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["params"], fixture["request"]["params"]);
}

#[test]
fn each_jump_error_code_is_typed_and_no_jump_is_retried() {
    let daemon = MockDaemon::start("default");
    let codes = [
        "client_not_found",
        "client_not_reporting",
        "jump_superseded",
        "client_departed",
        "stale_boot",
        "jump_target_not_found",
        "jump_endpoint_unavailable",
        "jump_timeout",
        "invalid_params",
    ];
    for code in codes {
        daemon.set_jump_reply(json!({"error": {"code": code, "message": "the client's words"}}));
        let error = send(&jump_target(&daemon)).expect_err(code);
        let typed = match (&error, code) {
            (ClientJumpError::ClientNotFound, "client_not_found")
            | (ClientJumpError::ClientNotReporting, "client_not_reporting")
            | (ClientJumpError::Superseded, "jump_superseded")
            | (ClientJumpError::ClientDeparted, "client_departed")
            | (ClientJumpError::StaleBoot, "stale_boot")
            | (ClientJumpError::TargetNotFound, "jump_target_not_found")
            | (ClientJumpError::EndpointUnavailable, "jump_endpoint_unavailable")
            | (ClientJumpError::Timeout, "jump_timeout") => true,
            (ClientJumpError::Other(Error::Api(api)), "invalid_params") => {
                api.code == "invalid_params"
            }
            _ => false,
        };
        assert!(typed, "{code} became {error:?}");
    }
    assert_eq!(daemon.requests("client.jump").len(), codes.len());

    // No daemon at all is a transport failure, not a typed jump outcome.
    let socket_path = daemon.socket_path.clone();
    let mut gone = jump_target(&daemon);
    drop(daemon);
    gone.client.socket_path = socket_path;
    assert!(matches!(
        gone.start(),
        Err(ClientJumpError::Other(Error::Io(_)))
    ));
}

#[test]
fn a_jump_is_written_when_it_starts_and_answered_when_it_is_waited_for() {
    let daemon = MockDaemon::start("default");
    daemon.hold_jumps();
    let first = jump_target(&daemon);
    let mut second = jump_target(&daemon);
    second.pane_id = "w1:p9".into();

    // Neither start waits for an answer: the daemon has both requests, in
    // the order they were started, and has answered neither.
    let pending = [first.start().unwrap(), second.start().unwrap()];
    wait_requests(&daemon, "client.jump", 2);
    assert_eq!(
        daemon
            .requests("client.jump")
            .iter()
            .map(|request| request["params"]["pane_id"].clone())
            .collect::<Vec<_>>(),
        ["w1:p4", "w1:p9"]
    );

    daemon.set_jump_reply(json!({"error": {"code": "jump_superseded", "message": "replaced"}}));
    daemon.answer_jumps();
    for pending in pending {
        assert!(matches!(pending.wait(), Err(ClientJumpError::Superseded)));
    }
    assert_eq!(daemon.requests("client.jump").len(), 2);
}

#[test]
fn listing_clients_returns_the_daemon_boot_and_every_client() {
    let daemon = MockDaemon::start("default");
    daemon.set_client(client(2, Some(true)));
    daemon.set_client(ClientInfo {
        client_id: 1,
        report: None,
    });
    let list = Client::new(&daemon.socket_path).list_clients().unwrap();
    assert_eq!(
        list,
        ClientList {
            boot_id: "boot-default".into(),
            clients: vec![
                ClientInfo {
                    client_id: 1,
                    report: None
                },
                client(2, Some(true))
            ],
        }
    );
}

#[test]
fn a_single_listing_reports_each_daemon_it_could_not_list_with_the_reason() {
    let current = MockDaemon::start("work");
    current.set_clients(vec![client(1, Some(false)), client(2, None)]);
    let rejecting = MockDaemon::start("default");
    rejecting.fail_listing(Some(
        json!({"code": "invalid_request", "message": "no such method"}),
    ));
    let gone = LocalSession {
        name: "gone".into(),
        socket_path: "/tmp/hc-definitely-missing/herdr.sock".into(),
    };
    let mut failures = Vec::new();
    let directory = follow::list_once(
        &[current.session(), rejecting.session(), gone],
        Duration::from_secs(2),
        |session, error| failures.push((session.name.clone(), error)),
    );
    let state = |session: &str| {
        directory
            .daemons
            .iter()
            .find(|daemon| daemon.session == session)
            .map(|daemon| (&daemon.state, daemon.clients.len()))
            .unwrap()
    };
    assert_eq!(
        state("work"),
        (
            &DaemonState::Following {
                boot_id: "boot-work".into()
            },
            2
        )
    );
    assert_eq!(state("default"), (&DaemonState::Offline, 0));
    assert_eq!(state("gone"), (&DaemonState::Offline, 0));
    assert_eq!(failures.len(), 2);
    assert!(matches!(
        &failures[0],
        (session, Error::Api(ApiError { code, message }))
            if session == "default" && code == "invalid_request" && message == "no such method"
    ));
    assert!(matches!(&failures[1], (session, Error::Io(_)) if session == "gone"));
    // One reading: the client a follower would pick with nothing followed.
    assert_eq!(
        directory.followed_client().map(|client| (
            client.session,
            client.client_id,
            client.has_focus()
        )),
        Some(("work", 2, false))
    );
    // Nothing subscribed: this is one request per daemon.
    assert!(current.requests("events.subscribe").is_empty());
}
