use std::fmt;
use std::path::PathBuf;

use crate::{EventEnvelope, PluginInvocationContext};

#[derive(Debug, Clone, PartialEq)]
pub struct Environment {
    pub is_herdr: bool,
    pub socket_path: Option<PathBuf>,
    pub bin_path: Option<PathBuf>,
    pub plugin_id: Option<String>,
    pub plugin_root: Option<PathBuf>,
    pub plugin_config_dir: Option<PathBuf>,
    pub plugin_state_dir: Option<PathBuf>,
    pub context: Option<PluginInvocationContext>,
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub pane_id: Option<String>,
    pub action_id: Option<String>,
    pub event_name: Option<String>,
    pub event: Option<EventEnvelope>,
    pub entrypoint_id: Option<String>,
    pub clicked_url: Option<String>,
    pub link_handler_id: Option<String>,
}

impl Environment {
    pub fn load() -> Result<Self, EnvironmentError> {
        Ok(Self {
            is_herdr: string_var("HERDR_ENV").as_deref() == Some("1"),
            socket_path: path_var("HERDR_SOCKET_PATH"),
            bin_path: path_var("HERDR_BIN_PATH"),
            plugin_id: string_var("HERDR_PLUGIN_ID"),
            plugin_root: path_var("HERDR_PLUGIN_ROOT"),
            plugin_config_dir: path_var("HERDR_PLUGIN_CONFIG_DIR"),
            plugin_state_dir: path_var("HERDR_PLUGIN_STATE_DIR"),
            context: json_var("HERDR_PLUGIN_CONTEXT_JSON")?,
            workspace_id: string_var("HERDR_WORKSPACE_ID"),
            tab_id: string_var("HERDR_TAB_ID"),
            pane_id: string_var("HERDR_PANE_ID"),
            action_id: string_var("HERDR_PLUGIN_ACTION_ID"),
            event_name: string_var("HERDR_PLUGIN_EVENT"),
            event: json_var("HERDR_PLUGIN_EVENT_JSON")?,
            entrypoint_id: string_var("HERDR_PLUGIN_ENTRYPOINT_ID"),
            clicked_url: string_var("HERDR_PLUGIN_CLICKED_URL"),
            link_handler_id: string_var("HERDR_PLUGIN_LINK_HANDLER_ID"),
        })
    }
}

#[derive(Debug)]
pub struct EnvironmentError {
    pub variable: &'static str,
    pub source: serde_json::Error,
}

impl fmt::Display for EnvironmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} contains invalid JSON: {}",
            self.variable, self.source
        )
    }
}

impl std::error::Error for EnvironmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn string_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn path_var(name: &str) -> Option<PathBuf> {
    string_var(name).map(PathBuf::from)
}

fn json_var<T: serde::de::DeserializeOwned>(
    name: &'static str,
) -> Result<Option<T>, EnvironmentError> {
    string_var(name)
        .map(|json| {
            serde_json::from_str(&json).map_err(|source| EnvironmentError {
                variable: name,
                source,
            })
        })
        .transpose()
}
