use std::{
    collections::HashMap,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use herdr_client::{
    AgentSessionInfo, AgentStartParams, Client, Environment, Error, PaneSplitParams, SplitDirection,
};
use serde_json::json;

const NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(2);
const SHELL_READY_TIMEOUT: Duration = Duration::from_secs(5);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const AMP_READY_TIMEOUT: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fork-to-pane: {error:#}");
            if let Ok(client) = Client::from_env() {
                let _ = client.with_timeout(NOTIFICATION_TIMEOUT).call_value(
                    "notification.show",
                    &json!({
                        "title": "Fork agent failed",
                        "body": format!("{error:#}"),
                    }),
                );
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let environment = Environment::load()?;
    let source_pane_id = environment
        .pane_id
        .as_deref()
        .context("HERDR_PANE_ID is not set")?;
    let client = Client::from_env()?;
    let source = client
        .current_pane(Some(source_pane_id))
        .context("read focused pane")?;
    let amp_parent = if source.agent.as_deref() == Some("amp") {
        Some(amp_thread_id(
            source.tokens.get("amp_thread_id").map(String::as_str),
        )?)
    } else {
        None
    };
    let (kind, args) = if amp_parent.is_some() {
        ("amp", vec![])
    } else {
        let session = source
            .agent_session
            .as_ref()
            .ok_or_else(|| anyhow!(missing_session_message(source.agent.as_deref())))?;
        fork_command(session)?
    };
    let pane = client
        .split_pane(&PaneSplitParams {
            workspace_id: None,
            target_pane_id: Some(source.pane_id),
            direction: SplitDirection::Right,
            ratio: None,
            cwd: source.foreground_cwd.or(source.cwd),
            focus: false,
            env: HashMap::new(),
        })
        .context("split pane")?;

    if let Err(error) = start_agent_when_shell_ready(
        &client,
        &AgentStartParams {
            name: format!("f-{}", pane.pane_id.to_ascii_lowercase().replace(':', "-")),
            kind: kind.into(),
            pane_id: pane.pane_id.clone(),
            args,
            timeout_ms: None,
        },
    ) {
        let message = format!("start {kind} fork: {error}");
        bail!(if start_was_rejected(&error) {
            rollback(&client, &pane.pane_id, message)
        } else {
            format!(
                "{message}; pane {} left open because launch outcome is unknown",
                pane.pane_id
            )
        });
    }

    if let Some(parent) = amp_parent {
        prefill_amp(&client, &pane.pane_id, parent).with_context(|| {
            format!(
                "Amp started in {}, but could not prefill its input; pane left open",
                pane.pane_id
            )
        })?;
    }

    client.focus_pane(&pane.pane_id).map_err(|error| {
        anyhow!(
            "{kind} fork started in {}, but focus failed: {error}",
            pane.pane_id
        )
    })?;
    Ok(())
}

fn prefill_amp(client: &Client, pane_id: &str, parent: &str) -> Result<()> {
    let deadline = Instant::now() + AMP_READY_TIMEOUT;
    loop {
        // agent.wait matches status alone; idle can precede interactive readiness.
        let response = client.call_value("agent.get", &json!({ "target": pane_id }))?;
        let agent = &response["agent"];
        if agent["agent"] == "amp"
            && agent["interactive_ready"] == true
            && matches!(agent["agent_status"].as_str(), Some("idle" | "done"))
        {
            break;
        }
        if agent["launch_pending"] != true {
            bail!("Amp input is not ready");
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for Amp input to be ready");
        }
        thread::sleep(READY_POLL_INTERVAL);
    }
    client.call_value(
        "pane.send_text",
        &json!({ "pane_id": pane_id, "text": format!("@{parent} ") }),
    )?;
    Ok(())
}

fn start_agent_when_shell_ready(client: &Client, params: &AgentStartParams) -> Result<(), Error> {
    let deadline = Instant::now() + SHELL_READY_TIMEOUT;
    loop {
        match client.start_agent(params) {
            Ok(_) => return Ok(()),
            Err(error) if shell_is_starting(&error) && Instant::now() < deadline => {
                thread::sleep(READY_POLL_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
}

fn shell_is_starting(error: &Error) -> bool {
    matches!(error, Error::Api(error) if error.code == "agent_pane_busy")
}

fn start_was_rejected(error: &Error) -> bool {
    matches!(error, Error::Api(_))
}

fn amp_thread_id(value: Option<&str>) -> Result<&str> {
    let value = value.context(
        "no active Amp thread reference; install fork-to-pane/amp-plugin.ts in ~/.config/amp/plugins/, reload Amp plugins, and open a thread",
    )?;
    if value.len() != 38
        || !value.starts_with("T-")
        || !value[2..].bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        bail!("Amp companion reported an invalid thread ID");
    }
    Ok(value)
}

fn fork_command(session: &AgentSessionInfo) -> Result<(&'static str, Vec<String>)> {
    if session.value.is_empty() {
        bail!("focused agent reported an empty session reference");
    }
    let value = session.value.clone();
    match (
        session.source.as_str(),
        session.agent.as_str(),
        session.kind.as_str(),
    ) {
        ("herdr:pi", "pi", "path" | "id") => Ok(("pi", vec!["--fork".into(), value])),
        ("herdr:codex", "codex", "id") => Ok(("codex", vec!["fork".into(), value])),
        ("herdr:claude", "claude", "id") => Ok((
            "claude",
            vec!["--resume".into(), value, "--fork-session".into()],
        )),
        ("herdr:opencode", "opencode", "id") => {
            Ok(("opencode", vec!["--session".into(), value, "--fork".into()]))
        }
        (_, agent, _) if matches!(agent, "pi" | "codex" | "claude" | "opencode") => Err(anyhow!(
            "unsupported {agent} session reference from {} ({})",
            session.source,
            session.kind
        )),
        (_, agent, _) => Err(anyhow!(unsupported_agent_message(agent))),
    }
}

fn missing_session_message(agent: Option<&str>) -> String {
    match agent {
        Some(agent @ ("pi" | "codex" | "claude" | "opencode")) => {
            format!("no native {agent} session reference; run `herdr integration install {agent}`")
        }
        Some(agent) => unsupported_agent_message(agent),
        None => "focused pane is not running a supported agent".into(),
    }
}

fn unsupported_agent_message(agent: &str) -> String {
    match agent {
        "warp" => "Warp supports interactive /fork, but not a startup fork command with a Herdr native session reference; forking into another Herdr pane is not supported".into(),
        _ => format!("focused agent {agent:?} is not supported; expected pi, codex, claude, opencode, or amp"),
    }
}

fn rollback(client: &Client, pane_id: &str, error: String) -> String {
    match client.close_pane(pane_id) {
        Ok(()) => error,
        Err(close_error) => format!("{error}; also failed to close empty pane: {close_error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn amp_prefill_waits_for_interactive_readiness_before_pasting_once() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let path = std::env::temp_dir().join(format!("fork-amp-ready-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            for (method, result) in [
                (
                    "agent.get",
                    json!({ "agent": {
                        "agent": null, "agent_status": "idle",
                        "launch_pending": true, "interactive_ready": false,
                    }}),
                ),
                (
                    "agent.get",
                    json!({ "agent": {
                        "agent": "amp", "agent_status": "idle",
                        "launch_pending": true, "interactive_ready": false,
                    }}),
                ),
                (
                    "agent.get",
                    json!({ "agent": {
                        "agent": "amp", "agent_status": "idle",
                        "launch_pending": false, "interactive_ready": true,
                    }}),
                ),
                ("pane.send_text", json!({})),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], method);
                if method == "pane.send_text" {
                    assert_eq!(
                        request["params"],
                        json!({
                            "pane_id": "w1:p2", "text": "@T-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee ",
                        })
                    );
                } else {
                    assert_eq!(request["params"], json!({ "target": "w1:p2" }));
                }
                writeln!(
                    stream,
                    "{}",
                    json!({ "id": request["id"], "result": result })
                )
                .unwrap();
            }
        });
        let result = prefill_amp(
            &Client::new(&path).with_timeout(Duration::from_secs(2)),
            "w1:p2",
            "T-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
        );
        result.unwrap();
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    fn session(source: &str, agent: &str, kind: &str, value: &str) -> AgentSessionInfo {
        AgentSessionInfo {
            source: source.into(),
            agent: agent.into(),
            kind: kind.into(),
            value: value.into(),
        }
    }

    #[test]
    fn maps_native_sessions_to_real_forks() {
        assert_eq!(
            fork_command(&session("herdr:pi", "pi", "path", "/tmp/a b.jsonl")).unwrap(),
            ("pi", vec!["--fork".into(), "/tmp/a b.jsonl".into()])
        );
        assert_eq!(
            fork_command(&session("herdr:codex", "codex", "id", "codex-id")).unwrap(),
            ("codex", vec!["fork".into(), "codex-id".into()])
        );
        assert_eq!(
            fork_command(&session("herdr:claude", "claude", "id", "claude-id")).unwrap(),
            (
                "claude",
                vec![
                    "--resume".into(),
                    "claude-id".into(),
                    "--fork-session".into()
                ]
            )
        );
        assert_eq!(
            fork_command(&session("herdr:opencode", "opencode", "id", "ses_123")).unwrap(),
            (
                "opencode",
                vec!["--session".into(), "ses_123".into(), "--fork".into()]
            )
        );
    }

    #[test]
    fn rejects_non_native_or_unsupported_sessions() {
        assert!(fork_command(&session("custom:pi", "pi", "id", "id")).is_err());
        assert!(fork_command(&session("herdr:cursor", "cursor", "id", "id")).is_err());
        assert!(fork_command(&session("herdr:pi", "pi", "path", "")).is_err());
        assert!(fork_command(&session("custom:opencode", "opencode", "id", "ses_123")).is_err());
        assert!(
            fork_command(&session(
                "herdr:opencode",
                "opencode",
                "path",
                "/tmp/session"
            ))
            .is_err()
        );
        assert!(fork_command(&session("herdr:opencode", "opencode", "id", "")).is_err());
    }

    #[test]
    fn missing_opencode_session_explains_integration_setup() {
        assert_eq!(
            missing_session_message(Some("opencode")),
            "no native opencode session reference; run `herdr integration install opencode`"
        );
    }

    #[test]
    fn amp_requires_a_valid_companion_thread_id() {
        let id = "T-12345678-1234-1234-1234-123456789abc";
        assert_eq!(amp_thread_id(Some(id)).unwrap(), id);
        assert!(
            amp_thread_id(None)
                .unwrap_err()
                .to_string()
                .contains("amp-plugin.ts")
        );
        for invalid in [
            "",
            "T-not-a-uuid",
            "--continue",
            "T-12345678-1234-1234-1234-123456789abz",
        ] {
            assert!(amp_thread_id(Some(invalid)).is_err());
        }
    }

    #[test]
    fn warp_explains_fork_limitations() {
        assert!(missing_session_message(Some("warp")).contains("interactive /fork"));
        assert!(
            fork_command(&session("herdr:warp", "warp", "id", "id"))
                .unwrap_err()
                .to_string()
                .contains("interactive /fork")
        );
    }

    #[test]
    fn only_pane_busy_is_a_shell_startup_race() {
        assert!(shell_is_starting(&Error::Api(herdr_client::ApiError {
            code: "agent_pane_busy".into(),
            message: "not ready".into(),
        })));
        assert!(!shell_is_starting(&Error::Api(herdr_client::ApiError {
            code: "agent_name_taken".into(),
            message: "duplicate".into(),
        })));
    }

    #[test]
    fn only_explicit_rejections_are_safe_to_rollback() {
        assert!(start_was_rejected(&Error::Api(herdr_client::ApiError {
            code: "agent_name_taken".into(),
            message: "duplicate".into(),
        })));
        assert!(!start_was_rejected(&Error::MissingResult));
    }
}
