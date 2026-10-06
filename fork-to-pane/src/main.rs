use std::{collections::HashMap, process::ExitCode, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use herdr_client::{
    AgentSessionInfo, AgentStartParams, Client, Environment, Error, PaneSplitParams, SplitDirection,
};
use serde_json::json;

const NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(2);

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
    let source = Environment::load()?
        .context
        .context("HERDR_PLUGIN_CONTEXT_JSON is not set")?;
    let source_pane_id = source
        .focused_pane_id
        .context("the invocation names no pane")?;
    let session = source.focused_pane_agent_session.as_ref().ok_or_else(|| {
        anyhow!(missing_session_message(
            source.focused_pane_agent.as_deref()
        ))
    })?;
    let (kind, args) = fork_command(session)?;
    let client = Client::from_env()?;
    let pane = client
        .split_pane(&PaneSplitParams {
            workspace_id: None,
            target_pane_id: Some(source_pane_id),
            direction: SplitDirection::Right,
            ratio: None,
            cwd: source
                .focused_pane_foreground_cwd
                .or(source.focused_pane_cwd),
            focus: false,
            env: HashMap::new(),
        })
        .context("split pane")?;

    // Herdr waits for the new pane's shell to finish starting.
    if let Err(error) = client.start_agent(&AgentStartParams {
        name: format!("f-{}", pane.pane_id.to_ascii_lowercase().replace(':', "-")),
        kind: kind.into(),
        pane_id: pane.pane_id.clone(),
        args,
        timeout_ms: None,
    }) {
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

    client.focus_pane(&pane.pane_id).map_err(|error| {
        anyhow!(
            "{kind} fork started in {}, but focus failed: {error}",
            pane.pane_id
        )
    })?;
    Ok(())
}

fn start_was_rejected(error: &Error) -> bool {
    matches!(error, Error::Api(_))
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
    format!("focused agent {agent:?} is not supported; expected pi, codex, claude, or opencode")
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
    fn only_explicit_rejections_are_safe_to_rollback() {
        assert!(start_was_rejected(&Error::Api(herdr_client::ApiError {
            code: "agent_name_taken".into(),
            message: "duplicate".into(),
        })));
        assert!(!start_was_rejected(&Error::MissingResult));
    }
}
