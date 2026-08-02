use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::thread;
use std::time::Duration;

use crate::herdr::{herdr_bin, run_json, session_environment, Environment};

pub const GHOSTTY_STATE_SCRIPT: &str = r#"
const app = Application("Ghostty");
const frontmost = app.frontmost();
const terminals = app.terminals().map((terminal) => ({
  id: terminal.id(),
  name: terminal.name(),
}));
let focusedTerminalId = null;
if (frontmost && app.windows().length > 0) {
  const tab = app.windows()[0].selectedTab();
  focusedTerminalId = tab.focusedTerminal().id();
}
JSON.stringify({ frontmost, focusedTerminalId, terminals });
"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhosttyTerminal {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhosttyState {
    pub frontmost: bool,
    pub focused_terminal_id: Option<String>,
    pub terminals: Vec<GhosttyTerminal>,
}

pub fn parse_ghostty_state(stdout: &str) -> Result<GhosttyState> {
    let state: GhosttyState =
        serde_json::from_str(stdout).context("Ghostty returned invalid terminal state")?;
    if state
        .terminals
        .iter()
        .any(|terminal| terminal.id.is_empty())
    {
        bail!("Ghostty returned invalid terminal state");
    }
    Ok(state)
}

pub fn inspect_ghostty() -> Result<GhosttyState> {
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", GHOSTTY_STATE_SCRIPT])
        .output()
        .context("failed to inspect Ghostty")?;
    if !output.status.success() {
        bail!(
            "Ghostty inspection failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_ghostty_state(&String::from_utf8_lossy(&output.stdout))
}

fn set_session_title(session_name: &str, title: &str, base: &Environment) -> Result<()> {
    let args = vec![
        "terminal".into(),
        "title".into(),
        "set".into(),
        title.into(),
    ];
    let value = run_json(
        &herdr_bin(),
        &args,
        Some(&session_environment(session_name, base)),
    )?;
    if value.pointer("/result/changed").and_then(Value::as_bool) != Some(true) {
        bail!("Herdr session {session_name} did not change a terminal title");
    }
    Ok(())
}

fn find_token<I>(inspect: &mut I, token: &str) -> Result<Option<GhosttyTerminal>>
where
    I: FnMut() -> Result<GhosttyState>,
{
    for attempt in 0..10 {
        let state = inspect()?;
        let matches: Vec<_> = state
            .terminals
            .into_iter()
            .filter(|terminal| terminal.name == token)
            .collect();
        match matches.len() {
            0 => {
                if attempt != 9 {
                    thread::sleep(Duration::from_millis(50));
                }
            }
            1 => return Ok(matches.into_iter().next()),
            _ => bail!("Ghostty exposed duplicate probe token {token}"),
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTerminalMapping {
    pub session_name: String,
    pub terminal_id: String,
}

pub fn probe_session_terminals_with<I, S, T>(
    sessions: &[String],
    mut inspect: I,
    mut set_title: S,
    mut create_token: T,
) -> Result<Vec<SessionTerminalMapping>>
where
    I: FnMut() -> Result<GhosttyState>,
    S: FnMut(&str, &str) -> Result<()>,
    T: FnMut(&str) -> String,
{
    let mut mappings = Vec::new();
    for session_name in sessions {
        let before = inspect()?;
        let originals: std::collections::BTreeMap<_, _> = before
            .terminals
            .iter()
            .map(|terminal| (terminal.id.clone(), terminal.name.clone()))
            .collect();
        let token = create_token(session_name);
        set_title(session_name, &token)?;

        let terminal = find_token(&mut inspect, &token)?
            .ok_or_else(|| anyhow!("Herdr session {session_name} did not appear in Ghostty"))?;
        let original = originals
            .get(&terminal.id)
            .ok_or_else(|| anyhow!("Ghostty topology changed while probing {session_name}"))?
            .clone();
        let duplicate = mappings
            .iter()
            .any(|mapping: &SessionTerminalMapping| mapping.terminal_id == terminal.id);

        // After identifying a terminal, restore and verify its title before
        // reporting a duplicate mapping.
        set_title(session_name, &original)?;
        if inspect()?
            .terminals
            .iter()
            .find(|candidate| candidate.id == terminal.id)
            .map(|candidate| &candidate.name)
            != Some(&original)
        {
            bail!("failed to restore {session_name} terminal title");
        }
        if duplicate {
            bail!(
                "multiple Herdr sessions targeted Ghostty terminal {}",
                terminal.id
            );
        }
        mappings.push(SessionTerminalMapping {
            session_name: session_name.clone(),
            terminal_id: terminal.id,
        });
    }
    Ok(mappings)
}

pub fn probe_session_terminals(
    sessions: &[String],
    base: &Environment,
) -> Result<Vec<SessionTerminalMapping>> {
    probe_session_terminals_with(
        sessions,
        inspect_ghostty,
        |session, title| set_session_title(session, title, base),
        |session| format!("__herdr_micro_{session}_{}__", unique_token()),
    )
}

fn unique_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!(
        "{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

pub fn focused_session(
    mappings: &[SessionTerminalMapping],
    state: &GhosttyState,
) -> Option<String> {
    if !state.frontmost {
        return None;
    }
    let id = state.focused_terminal_id.as_deref()?;
    mappings
        .iter()
        .find(|mapping| mapping.terminal_id == id)
        .map(|mapping| mapping.session_name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn state(terminals: Vec<(&str, &str)>) -> GhosttyState {
        GhosttyState {
            frontmost: true,
            focused_terminal_id: Some("personal-terminal".into()),
            terminals: terminals
                .into_iter()
                .map(|(id, name)| GhosttyTerminal {
                    id: id.into(),
                    name: name.into(),
                })
                .collect(),
        }
    }

    #[test]
    fn probes_named_sessions_and_restores_titles() {
        let terminals = RefCell::new(vec![
            ("personal-terminal".to_owned(), "pers @ herdr".to_owned()),
            ("work-terminal".to_owned(), "werk @ herdr".to_owned()),
        ]);
        let sessions = vec!["default".into(), "werk".into()];
        let mappings = probe_session_terminals_with(
            &sessions,
            || {
                Ok(state(
                    terminals
                        .borrow()
                        .iter()
                        .map(|(id, name)| (id.as_str(), name.as_str()))
                        .collect(),
                ))
            },
            |session, title| {
                let id = if session == "default" {
                    "personal-terminal"
                } else {
                    "work-terminal"
                };
                terminals
                    .borrow_mut()
                    .iter_mut()
                    .find(|(candidate, _)| candidate == id)
                    .unwrap()
                    .1 = title.into();
                Ok(())
            },
            |session| format!("probe-{session}"),
        )
        .unwrap();
        assert_eq!(
            mappings,
            vec![
                SessionTerminalMapping {
                    session_name: "default".into(),
                    terminal_id: "personal-terminal".into()
                },
                SessionTerminalMapping {
                    session_name: "werk".into(),
                    terminal_id: "work-terminal".into()
                }
            ]
        );
        assert_eq!(terminals.into_inner()[0].1, "pers @ herdr");
    }

    #[test]
    fn validates_state_and_only_routes_frontmost_terminal() {
        let state = parse_ghostty_state(
            r#"{"frontmost":true,"focusedTerminalId":"p","terminals":[{"id":"p","name":"x"}]}"#,
        )
        .unwrap();
        let unfocused = parse_ghostty_state(r#"{"frontmost":false,"terminals":[]}"#).unwrap();
        assert_eq!(unfocused.focused_terminal_id, None);
        assert!(parse_ghostty_state(r#"{"frontmost":true,"terminals":null}"#).is_err());
        assert!(parse_ghostty_state(
            r#"{"frontmost":true,"focusedTerminalId":null,"terminals":[{"id":"","name":"x"}]}"#
        )
        .is_err());
        let mapping = vec![SessionTerminalMapping {
            session_name: "default".into(),
            terminal_id: "p".into(),
        }];
        assert_eq!(
            focused_session(&mapping, &state).as_deref(),
            Some("default")
        );
        assert_eq!(
            focused_session(
                &mapping,
                &GhosttyState {
                    frontmost: false,
                    ..state
                }
            ),
            None
        );
    }
}
