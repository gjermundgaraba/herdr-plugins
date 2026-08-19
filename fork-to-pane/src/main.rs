use std::{
    collections::HashMap,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use herdr_client::{
    AgentSessionInfo, AgentStartParams, Client, Environment, Error, PaneSplitParams, SplitDirection,
};
use serde_json::json;

const NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(2);
const SHELL_READY_TIMEOUT: Duration = Duration::from_secs(5);
const SHELL_READY_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fork-to-pane: {error}");
            if let Ok(client) = Client::from_env() {
                let _ = client.with_timeout(NOTIFICATION_TIMEOUT).call_value(
                    "notification.show",
                    &json!({
                        "title": "Fork agent failed",
                        "body": error,
                    }),
                );
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let environment = Environment::load().map_err(|error| error.to_string())?;
    let source_pane_id = environment
        .pane_id
        .as_deref()
        .ok_or("HERDR_PANE_ID is not set")?;
    let client = Client::from_env().map_err(|error| error.to_string())?;
    let source = client
        .current_pane(Some(source_pane_id))
        .map_err(|error| format!("read focused pane: {error}"))?;
    let session = source
        .agent_session
        .as_ref()
        .ok_or_else(|| missing_session_message(source.agent.as_deref()))?;
    let (kind, args) = fork_command(session)?;
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
        .map_err(|error| format!("split pane: {error}"))?;

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
        return Err(if start_was_rejected(&error) {
            rollback(&client, &pane.pane_id, message)
        } else {
            format!(
                "{message}; pane {} left open because launch outcome is unknown",
                pane.pane_id
            )
        });
    }

    client.focus_pane(&pane.pane_id).map_err(|error| {
        format!(
            "{kind} fork started in {}, but focus failed: {error}",
            pane.pane_id
        )
    })?;
    Ok(())
}

fn start_agent_when_shell_ready(client: &Client, params: &AgentStartParams) -> Result<(), Error> {
    let deadline = Instant::now() + SHELL_READY_TIMEOUT;
    loop {
        match client.start_agent(params) {
            Ok(_) => return Ok(()),
            Err(error) if shell_is_starting(&error) && Instant::now() < deadline => {
                thread::sleep(SHELL_READY_POLL_INTERVAL);
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

fn fork_command(session: &AgentSessionInfo) -> Result<(&'static str, Vec<String>), String> {
    if session.value.is_empty() {
        return Err("focused agent reported an empty session reference".into());
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
        (_, agent, _) if matches!(agent, "pi" | "codex" | "claude") => Err(format!(
            "unsupported {agent} session reference from {} ({})",
            session.source, session.kind
        )),
        (_, agent, _) => Err(unsupported_agent_message(agent)),
    }
}

fn missing_session_message(agent: Option<&str>) -> String {
    match agent {
        Some(agent @ ("pi" | "codex" | "claude")) => {
            format!("no native {agent} session reference; run `herdr integration install {agent}`")
        }
        Some(agent) => unsupported_agent_message(agent),
        None => "focused pane is not running a supported agent".into(),
    }
}

fn unsupported_agent_message(agent: &str) -> String {
    format!("focused agent {agent:?} is not supported; expected pi, codex, or claude")
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
    }

    #[test]
    fn rejects_non_native_or_unsupported_sessions() {
        assert!(fork_command(&session("custom:pi", "pi", "id", "id")).is_err());
        assert!(fork_command(&session("herdr:cursor", "cursor", "id", "id")).is_err());
        assert!(fork_command(&session("herdr:pi", "pi", "path", "")).is_err());
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
