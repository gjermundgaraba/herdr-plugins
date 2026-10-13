use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use super::*;
use crate::ApiError;

const STOP: &str = "test.stop";
/// A short retry budget for tests that run it out.
const BUDGET: Duration = Duration::from_millis(300);
/// How often a dripping answer sends another piece, well within the budget.
const DRIP_GAP: Duration = Duration::from_millis(20);

/// How a test daemon writes an answer.
enum Reply {
    /// The whole answer at once; [`hang_up`] closes the connection unanswered.
    Now(Value),
    /// A space, which JSON allows before the answer, every [`DRIP_GAP`]
    /// until the receiver gets a message, then the answer. Each piece
    /// arrives before a socket timeout could expire, so only an absolute
    /// deadline ends the read.
    Drip(Value, mpsc::Receiver<()>),
}

impl From<Value> for Reply {
    fn from(answer: Value) -> Self {
        Self::Now(answer)
    }
}

/// A daemon that answers one request per connection and records the
/// methods it was asked. While `answer` runs, the request's connection stays
/// open and unanswered.
struct Daemon {
    dir: tempfile::TempDir,
    path: PathBuf,
    server: JoinHandle<Vec<String>>,
}

impl Daemon {
    fn start<R: Into<Reply>>(mut answer: impl FnMut(&str) -> R + Send + 'static) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let mut methods = Vec::new();
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                if method == STOP {
                    return methods;
                }
                let (mut response, release) = match answer(&method).into() {
                    Reply::Now(response) => (response, None),
                    Reply::Drip(response, release) => (response, Some(release)),
                };
                if !response.is_null() {
                    response["id"] = request["id"].clone();
                    let line = format!("{response}\n");
                    match release {
                        None => stream.write_all(line.as_bytes()).unwrap(),
                        // The client may have hung up by now, so writes may fail.
                        Some(release) => {
                            while release.recv_timeout(DRIP_GAP).is_err() {
                                let _ = stream.write_all(b" ");
                            }
                            let _ = stream.write_all(line.as_bytes());
                        }
                    }
                }
                methods.push(method);
            }
        });
        Self { dir, path, server }
    }

    fn client(&self) -> Client {
        Client::new(&self.path)
    }

    /// Stops the daemon and returns the methods it was asked, in order.
    fn finish(self) -> Vec<String> {
        let mut stream = UnixStream::connect(&self.path).unwrap();
        writeln!(stream, "{}", json!({ "id": "stop", "method": STOP })).unwrap();
        let methods = self.server.join().unwrap();
        self.dir.close().unwrap();
        methods
    }
}

fn params() -> AgentStartParams {
    AgentStartParams {
        name: "fork".into(),
        kind: "pi".into(),
        pane_id: "w1:p2".into(),
        args: Vec::new(),
        timeout_ms: None,
    }
}

fn pane(terminal_id: &str) -> Value {
    json!({ "result": { "type": "pane_info", "pane": { "terminal_id": terminal_id } } })
}

fn error(code: &str) -> Value {
    json!({ "error": { "code": code, "message": code } })
}

/// Closes the connection without an answer, a transport error to the client.
fn hang_up() -> Value {
    Value::Null
}

fn start_calls(methods: &[String]) -> usize {
    methods
        .iter()
        .filter(|method| *method == "agent.start")
        .count()
}

fn process_info(group: u64, processes: Value) -> Value {
    json!({ "result": { "type": "pane_process_info", "process_info": {
        "pane_id": "w1:p2",
        "shell_pid": 10,
        "foreground_process_group_id": group,
        "foreground_processes": processes,
    } } })
}

fn starting() -> Value {
    process_info(
        10,
        json!([{ "pid": 10, "name": "bash" }, { "pid": 11, "name": "startup-helper" }]),
    )
}

fn started() -> Value {
    json!({ "result": { "type": "agent_started", "agent": {
        "terminal_id": "t1", "name": "fork", "agent": "pi", "agent_status": "unknown",
        "workspace_id": "w1", "tab_id": "w1:t1", "pane_id": "w1:p2", "focused": false,
        "revision": 1,
    } } })
}

fn busy_code(result: Result<AgentInfo, Error>) -> String {
    match result {
        Err(Error::Api(ApiError { code, .. })) => code,
        other => panic!("expected an API error, got {other:?}"),
    }
}

#[test]
fn retries_a_starting_shell_until_the_agent_starts() {
    let mut starts = 0;
    let daemon = Daemon::start(move |method| match method {
        "pane.get" => pane("t1"),
        "pane.process_info" => starting(),
        "agent.start" => {
            starts += 1;
            if starts < 3 {
                error(PANE_BUSY)
            } else {
                started()
            }
        }
        other => panic!("unexpected {other}"),
    });
    let agent = daemon.client().start_agent(&params()).unwrap();
    assert_eq!(agent.pane_id, "w1:p2");
    assert_eq!(start_calls(&daemon.finish()), 3);
}

#[test]
fn a_start_that_succeeds_at_once_makes_no_other_call() {
    let daemon = Daemon::start(|method| match method {
        "agent.start" => started(),
        other => panic!("unexpected {other}"),
    });
    daemon.client().start_agent(&params()).unwrap();
    assert_eq!(daemon.finish(), ["agent.start"]);
}

/// The pane gets another terminal after the second busy answer. A start
/// that followed the change would succeed, so the busy answer shows that none
/// did.
#[test]
fn stops_when_the_pane_gets_another_terminal() {
    let mut starts = 0;
    let daemon = Daemon::start(move |method| match method {
        "pane.get" => pane(if starts < 2 { "t1" } else { "t2" }),
        "pane.process_info" => starting(),
        "agent.start" => {
            starts += 1;
            if starts <= 2 {
                error(PANE_BUSY)
            } else {
                started()
            }
        }
        other => panic!("unexpected {other}"),
    });
    assert_eq!(busy_code(daemon.client().start_agent(&params())), PANE_BUSY);
    assert_eq!(start_calls(&daemon.finish()), 2);
}

#[test]
fn does_not_retry_a_shell_running_a_command() {
    let daemon = Daemon::start(|method| match method {
        "pane.get" => pane("t1"),
        "pane.process_info" => process_info(20, json!([{ "pid": 20, "name": "vim" }])),
        "agent.start" => error(PANE_BUSY),
        other => panic!("unexpected {other}"),
    });
    assert_eq!(busy_code(daemon.client().start_agent(&params())), PANE_BUSY);
    assert_eq!(start_calls(&daemon.finish()), 1);
}

#[test]
fn gives_up_two_seconds_after_the_first_busy_answer() {
    let daemon = Daemon::start(|method| match method {
        "pane.get" => pane("t1"),
        "pane.process_info" => starting(),
        "agent.start" => error(PANE_BUSY),
        other => panic!("unexpected {other}"),
    });
    let started_at = Instant::now();
    assert_eq!(busy_code(daemon.client().start_agent(&params())), PANE_BUSY);
    let elapsed = started_at.elapsed();
    assert!(elapsed >= SHELL_READINESS_RETRY_TIMEOUT, "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
    let starts = start_calls(&daemon.finish());
    assert!(starts > 1, "{starts} starts");
}

/// A read that never answers runs out with the budget instead of holding
/// the caller, which then gets the busy answer and can roll back.
#[test]
fn a_stalled_read_returns_the_busy_answer_within_the_budget() {
    for stalled in ["pane.get", "pane.process_info"] {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let daemon = Daemon::start(move |method| match method {
            _ if method == stalled => {
                entered_tx.send(()).unwrap();
                // Held open until the test has the client's result.
                release_rx.recv().unwrap();
                hang_up()
            }
            "pane.get" => pane("t1"),
            "pane.process_info" => starting(),
            "agent.start" => error(PANE_BUSY),
            other => panic!("unexpected {other}"),
        });
        let client = daemon.client();
        let (done_tx, done_rx) = mpsc::channel();
        let started_at = Instant::now();
        let caller = thread::spawn(move || {
            done_tx
                .send(client.start_agent_within(&params(), BUDGET))
                .unwrap();
        });
        entered_rx.recv().unwrap();
        let result = done_rx.recv_timeout(BUDGET + Duration::from_secs(2));
        let elapsed = started_at.elapsed();
        release_tx.send(()).unwrap();
        let result = result.expect("start_agent waited on a stalled read");
        assert_eq!(busy_code(result), PANE_BUSY, "{stalled}");
        assert!(elapsed >= BUDGET, "{stalled}: {elapsed:?}");
        caller.join().unwrap();
        assert_eq!(start_calls(&daemon.finish()), 1, "{stalled}");
    }
}

/// A read whose answer keeps arriving in pieces ends at the deadline, and
/// the caller gets the busy answer before the daemon finishes it. The second
/// `pane.process_info` is the read right before another start.
#[test]
fn a_read_answered_piece_by_piece_ends_at_the_deadline() {
    for (dripping, nth) in [
        ("pane.get", 1),
        ("pane.process_info", 1),
        ("pane.process_info", 2),
    ] {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut release_rx = Some(release_rx);
        let mut calls = 0;
        let daemon = Daemon::start(move |method| {
            let answer = match method {
                "pane.get" => pane("t1"),
                "pane.process_info" => starting(),
                "agent.start" => error(PANE_BUSY),
                other => panic!("unexpected {other}"),
            };
            if method != dripping {
                return answer.into();
            }
            calls += 1;
            if calls != nth {
                return answer.into();
            }
            entered_tx.send(()).unwrap();
            Reply::Drip(answer, release_rx.take().unwrap())
        });
        let client = daemon.client();
        let (done_tx, done_rx) = mpsc::channel();
        let started_at = Instant::now();
        let caller = thread::spawn(move || {
            done_tx
                .send(client.start_agent_within(&params(), BUDGET))
                .unwrap();
        });
        entered_rx.recv().unwrap();
        let result = done_rx.recv_timeout(BUDGET + Duration::from_secs(2));
        let elapsed = started_at.elapsed();
        // Only now does the daemon finish its answer.
        release_tx.send(()).unwrap();
        let result = result.expect("start_agent waited for the whole answer");
        assert_eq!(busy_code(result), PANE_BUSY, "{dripping} #{nth}");
        assert!(elapsed >= BUDGET, "{dripping} #{nth}: {elapsed:?}");
        caller.join().unwrap();
        assert_eq!(start_calls(&daemon.finish()), 1, "{dripping} #{nth}");
    }
}

#[test]
fn retries_only_a_busy_answer_from_a_pane_with_a_terminal() {
    for (answer, pane_answer) in [
        ("agent_name_taken", pane("t1")),
        (PANE_BUSY, error("pane_not_found")),
    ] {
        let daemon = Daemon::start(move |method| match method {
            "pane.get" => pane_answer.clone(),
            "agent.start" => error(answer),
            other => panic!("unexpected {other}"),
        });
        assert_eq!(busy_code(daemon.client().start_agent(&params())), answer);
        assert_eq!(start_calls(&daemon.finish()), 1);
    }
}

/// A read between attempts launches nothing, so its transport error ends
/// the retry with the busy answer, which a caller can treat as a rejected
/// start.
#[test]
fn a_failed_read_returns_the_busy_answer() {
    for failing in ["pane.get", "pane.process_info"] {
        let daemon = Daemon::start(move |method| match method {
            _ if method == failing => hang_up(),
            "pane.get" => pane("t1"),
            "pane.process_info" => starting(),
            "agent.start" => error(PANE_BUSY),
            other => panic!("unexpected {other}"),
        });
        assert_eq!(busy_code(daemon.client().start_agent(&params())), PANE_BUSY);
        assert_eq!(start_calls(&daemon.finish()), 1);
    }
}

#[test]
fn a_shell_that_leads_its_job_is_starting() {
    let info = |group: u64, processes: Value| {
        process_info(group, processes)["result"]["process_info"].clone()
    };
    for name in ["bash", "-zsh", "/bin/fish"] {
        assert!(process_info_shows_shell_initialization(&info(
            10,
            json!([{ "pid": 10, "name": name }]),
        )));
    }
    assert!(process_info_shows_shell_initialization(&info(
        10,
        json!([{ "pid": 10, "name": "login", "argv": ["-zsh"] }]),
    )));
    for not_starting in [
        info(20, json!([{ "pid": 20, "name": "vim" }])),
        info(10, json!([{ "pid": 10, "name": "sleep" }])),
        info(10, json!([{ "pid": 11, "name": "bash" }])),
        Value::Null,
    ] {
        assert!(!process_info_shows_shell_initialization(&not_starting));
    }
}
