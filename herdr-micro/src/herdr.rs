//! Small, CLI-only Herdr client helpers.  The bridge deliberately does not
//! retain a socket connection: every call is scoped to one selected session.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::process::Command;

pub const DEFAULT_HERDR_BIN: &str = "herdr";

pub type Environment = BTreeMap<String, String>;

pub fn herdr_bin() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| DEFAULT_HERDR_BIN.to_owned())
}

/// Caller routing context must not leak into session discovery. Plugin config
/// variables intentionally survive because they are not caller context.
pub fn is_routing_context(key: &str) -> bool {
    key == "HERDR_SOCKET_PATH"
        || key == "HERDR_SESSION"
        || matches!(key, "HERDR_PANE_ID" | "HERDR_TAB_ID" | "HERDR_WORKSPACE_ID")
        || key.starts_with("HERDR_ACTIVE_")
        || key.starts_with("HERDR_PLUGIN_CONTEXT")
        || key.starts_with("HERDR_PLUGIN_ACTION")
        || key.starts_with("HERDR_PLUGIN_EVENT")
        || key.starts_with("HERDR_PLUGIN_ENTRYPOINT")
}

pub fn current_environment() -> Environment {
    env::vars().collect()
}

pub fn discovery_environment(base: &Environment) -> Environment {
    base.iter()
        .filter(|(key, _)| !is_routing_context(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub fn session_environment(session: &str, base: &Environment) -> Environment {
    let mut result = discovery_environment(base);
    result.insert("HERDR_SESSION".to_owned(), session.to_owned());
    result
}

pub fn run_command(bin: &str, args: &[String], env: Option<&Environment>) -> Result<String> {
    let mut command = Command::new(bin);
    command.args(args);
    if let Some(env) = env {
        command.env_clear().envs(env);
    }
    let output = command
        .output()
        .with_context(|| format!("failed to run {bin}"))?;
    let status = output.status.code().unwrap_or(-1);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        bail!(
            "{bin} {} exited with status {status}{}",
            args.join(" "),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn run_json(bin: &str, args: &[String], env: Option<&Environment>) -> Result<Value> {
    serde_json::from_str(&run_command(bin, args, env)?).context("Herdr returned invalid JSON")
}

pub fn discover_sessions_with<F>(base: &Environment, mut run: F) -> Result<Vec<String>>
where
    F: FnMut(&str, &[String], Option<&Environment>) -> Result<String>,
{
    let args = vec!["session".into(), "list".into(), "--json".into()];
    let output = run(&herdr_bin(), &args, Some(&discovery_environment(base)))?;
    let value = serde_json::from_str(&output).context("Herdr returned invalid JSON")?;
    #[derive(Deserialize)]
    struct Session {
        name: String,
        running: bool,
    }
    #[derive(Deserialize)]
    struct Sessions {
        sessions: Vec<Session>,
    }
    let sessions: Sessions = serde_json::from_value(value)
        .map_err(|_| anyhow!("Herdr returned invalid session list"))?;
    Ok(sessions
        .sessions
        .into_iter()
        .filter(|session| session.running)
        .map(|session| session.name)
        .collect())
}

pub fn discover_sessions(base: &Environment) -> Result<Vec<String>> {
    discover_sessions_with(base, run_command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_inherited_routing_context() {
        let base = Environment::from([
            ("PATH".into(), "/bin".into()),
            ("HERDR_CONFIG_PATH".into(), "/tmp/herdr.toml".into()),
            ("HERDR_SOCKET_PATH".into(), "/tmp/old.sock".into()),
            ("HERDR_SESSION".into(), "default".into()),
            ("HERDR_WORKSPACE_ID".into(), "w1".into()),
            ("HERDR_TAB_ID".into(), "w1:t1".into()),
            ("HERDR_PANE_ID".into(), "w1:p1".into()),
            ("HERDR_ACTIVE_PANE_CWD".into(), "/tmp".into()),
            ("HERDR_PLUGIN_CONTEXT_JSON".into(), "{}".into()),
            ("HERDR_PLUGIN_CONFIG_DIR".into(), "/tmp/config".into()),
        ]);
        let expected = Environment::from([
            ("PATH".into(), "/bin".into()),
            ("HERDR_CONFIG_PATH".into(), "/tmp/herdr.toml".into()),
            ("HERDR_PLUGIN_CONFIG_DIR".into(), "/tmp/config".into()),
        ]);
        assert_eq!(discovery_environment(&base), expected);
        let mut selected = expected;
        selected.insert("HERDR_SESSION".into(), "werk".into());
        assert_eq!(session_environment("werk", &base), selected);
    }

    #[test]
    fn session_discovery_keeps_running_names() {
        let base = Environment::new();
        let result = discover_sessions_with(&base, |_bin, _args, env| {
            assert!(!env.unwrap().contains_key("HERDR_SESSION"));
            Ok(r#"{"sessions":[{"name":"default","running":true},{"name":"old","running":false}]}"#.into())
        })
        .unwrap();
        assert_eq!(result, ["default"]);
    }
}
