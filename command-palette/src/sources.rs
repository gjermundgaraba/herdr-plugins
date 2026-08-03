use std::{cmp::Reverse, collections::HashMap, process::Command};

use herdr_client::{
    herdr_config_path, AgentStatus, Client, PluginInvocationContext, SessionSnapshot,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::model::{Dispatch, Item, Kind};

const PLUGIN_ID: &str = "gjermundgaraba.herdr-command-palette";
const SAFE_API_ACTIONS: [&str; 4] = [
    "swap_pane_left",
    "swap_pane_down",
    "swap_pane_up",
    "swap_pane_right",
];

#[derive(Debug, Deserialize)]
struct PluginActionList {
    #[serde(default)]
    actions: Vec<PluginAction>,
}

#[derive(Debug, Deserialize)]
struct PluginAction {
    plugin_id: String,
    action_id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone)]
struct NativeBinding {
    action: String,
    keys: Vec<String>,
}

pub fn collect(client: &Client) -> Result<Vec<Item>, String> {
    let snapshot = client.snapshot().map_err(|error| error.to_string())?;
    let context = invocation_context();
    let plugin_keys = load_plugin_keybindings();
    let mut items = Vec::new();

    items.extend(workspaces(&snapshot));
    items.extend(agents(&snapshot));
    items.extend(tabs(&snapshot));
    items.extend(panes(&snapshot));
    items.extend(native_actions(&snapshot, context.as_ref())?);
    items.extend(plugin_actions(client, context.as_ref(), &plugin_keys)?);

    Ok(items)
}

fn native_actions(
    snapshot: &SessionSnapshot,
    context: Option<&PluginInvocationContext>,
) -> Result<Vec<Item>, String> {
    Ok(load_native_bindings()?
        .into_iter()
        .filter_map(|binding| {
            let dispatch = native_dispatch(&binding.action, snapshot, context)?;
            Some(Item {
                kind: Kind::NativeAction,
                agent_status: None,
                title: humanize(&binding.action),
                subtitle: "Herdr native action".into(),
                detail: binding.action,
                keys: binding.keys,
                dispatch,
            })
        })
        .collect())
}

fn plugin_actions(
    client: &Client,
    context: Option<&PluginInvocationContext>,
    keybindings: &HashMap<String, Vec<String>>,
) -> Result<Vec<Item>, String> {
    let result: PluginActionList = client
        .call("plugin.action.list", &json!({}))
        .map_err(|error| error.to_string())?;
    Ok(result
        .actions
        .into_iter()
        .filter(|action| action.plugin_id != PLUGIN_ID)
        .map(|action| {
            let qualified = format!("{}.{}", action.plugin_id, action.action_id);
            Item {
                kind: Kind::PluginAction,
                agent_status: None,
                title: action.title,
                subtitle: action
                    .description
                    .unwrap_or_else(|| action.plugin_id.clone()),
                detail: qualified.clone(),
                keys: keybindings.get(&qualified).cloned().unwrap_or_default(),
                dispatch: Dispatch::new(
                    "plugin.action.invoke",
                    json!({
                        "action_id": qualified,
                        "context": context,
                    }),
                ),
            }
        })
        .collect())
}

fn workspaces(snapshot: &SessionSnapshot) -> Vec<Item> {
    let mut workspaces = snapshot.workspaces.clone();
    workspaces.sort_by_key(|workspace| workspace.number);
    workspaces
        .into_iter()
        .map(|workspace| Item {
            kind: Kind::Workspace,
            agent_status: None,
            title: workspace.label.clone(),
            subtitle: format!(
                "{} tabs · {} panes · {}",
                workspace.tab_count, workspace.pane_count, workspace.agent_status
            ),
            detail: workspace.workspace_id.clone(),
            keys: Vec::new(),
            dispatch: Dispatch::new(
                "workspace.focus",
                json!({ "workspace_id": workspace.workspace_id }),
            ),
        })
        .collect()
}

fn tabs(snapshot: &SessionSnapshot) -> Vec<Item> {
    let workspaces: HashMap<&str, &str> = snapshot
        .workspaces
        .iter()
        .map(|workspace| (workspace.workspace_id.as_str(), workspace.label.as_str()))
        .collect();
    let mut tabs = snapshot.tabs.clone();
    tabs.sort_by_key(|tab| {
        (
            snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == tab.workspace_id)
                .map(|workspace| workspace.number)
                .unwrap_or(usize::MAX),
            tab.number,
        )
    });
    tabs.into_iter()
        .map(|tab| {
            let workspace = workspaces
                .get(tab.workspace_id.as_str())
                .copied()
                .unwrap_or(tab.workspace_id.as_str());
            Item {
                kind: Kind::Tab,
                agent_status: None,
                title: tab.label.clone(),
                subtitle: format!(
                    "{workspace} · {} panes · {}",
                    tab.pane_count, tab.agent_status
                ),
                detail: tab.tab_id.clone(),
                keys: Vec::new(),
                dispatch: Dispatch::new("tab.focus", json!({ "tab_id": tab.tab_id })),
            }
        })
        .collect()
}

fn panes(snapshot: &SessionSnapshot) -> Vec<Item> {
    let workspace_labels: HashMap<&str, &str> = snapshot
        .workspaces
        .iter()
        .map(|workspace| (workspace.workspace_id.as_str(), workspace.label.as_str()))
        .collect();
    let tab_labels: HashMap<&str, &str> = snapshot
        .tabs
        .iter()
        .map(|tab| (tab.tab_id.as_str(), tab.label.as_str()))
        .collect();
    snapshot
        .panes
        .iter()
        .map(|pane| {
            let workspace = workspace_labels
                .get(pane.workspace_id.as_str())
                .copied()
                .unwrap_or(pane.workspace_id.as_str());
            let tab = tab_labels
                .get(pane.tab_id.as_str())
                .copied()
                .unwrap_or(pane.tab_id.as_str());
            let title = pane
                .title
                .as_deref()
                .or(pane.label.as_deref())
                .or(pane.display_agent.as_deref())
                .or(pane.agent.as_deref())
                .or(pane.terminal_title_stripped.as_deref())
                .unwrap_or(pane.pane_id.as_str());
            let cwd = pane
                .foreground_cwd
                .as_deref()
                .or(pane.cwd.as_deref())
                .unwrap_or("");
            Item {
                kind: Kind::Pane,
                agent_status: None,
                title: title.into(),
                subtitle: format!("{workspace} · {tab} · {}", pane.agent_status),
                detail: format!("{} · {} · {cwd}", pane.pane_id, pane.terminal_id),
                keys: Vec::new(),
                dispatch: Dispatch::new("pane.focus", json!({ "pane_id": pane.pane_id })),
            }
        })
        .collect()
}

fn agents(snapshot: &SessionSnapshot) -> Vec<Item> {
    let workspace_labels: HashMap<&str, &str> = snapshot
        .workspaces
        .iter()
        .map(|workspace| (workspace.workspace_id.as_str(), workspace.label.as_str()))
        .collect();
    let tab_labels: HashMap<&str, &str> = snapshot
        .tabs
        .iter()
        .map(|tab| (tab.tab_id.as_str(), tab.label.as_str()))
        .collect();
    let explicit_tab_labels = explicit_tab_labels(snapshot);

    let mut agents: Vec<_> = snapshot.agents.iter().collect();
    agents.sort_by_key(|agent| {
        (
            agent_status_priority(&agent.agent_status),
            Reverse(agent.state_change_seq),
        )
    });

    agents
        .into_iter()
        .map(|agent| {
            let workspace = workspace_labels
                .get(agent.workspace_id.as_str())
                .copied()
                .unwrap_or(agent.workspace_id.as_str());
            let tab = tab_labels
                .get(agent.tab_id.as_str())
                .copied()
                .unwrap_or(agent.tab_id.as_str());
            let explicit_tab = explicit_tab_labels.get(agent.tab_id.as_str()).copied();
            let stripped_terminal_title = nonempty(agent.terminal_title_stripped.as_deref());
            let terminal_title = nonempty(agent.terminal_title.as_deref());
            let base_title = [
                nonempty(agent.name.as_deref()),
                explicit_tab,
                stripped_terminal_title,
                terminal_title,
                Some(agent.terminal_id.as_str()),
            ]
            .into_iter()
            .flatten()
            .find(|candidate| *candidate != workspace);
            let title = base_title
                .map(|base_title| format!("{workspace}: {base_title}"))
                .unwrap_or_else(|| workspace.into());
            let cwd = nonempty(agent.foreground_cwd.as_deref())
                .or_else(|| nonempty(agent.cwd.as_deref()))
                .unwrap_or("");
            let subtitle = match (nonempty(Some(cwd)), nonempty(agent.agent.as_deref())) {
                (Some(path), Some(kind)) => format!("{path} · {kind}"),
                (Some(path), None) => path.into(),
                (None, Some(kind)) => kind.into(),
                (None, None) => String::new(),
            };
            Item {
                kind: Kind::Agent,
                agent_status: Some(agent.agent_status.clone()),
                title,
                subtitle,
                detail: [
                    agent.name.as_deref().unwrap_or(""),
                    agent.agent.as_deref().unwrap_or(""),
                    workspace,
                    tab,
                    agent.title.as_deref().unwrap_or(""),
                    stripped_terminal_title.or(terminal_title).unwrap_or(""),
                    cwd,
                    agent.agent_status.as_str(),
                    agent.terminal_id.as_str(),
                ]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
                keys: Vec::new(),
                dispatch: Dispatch::new("agent.focus", json!({ "target": agent.pane_id })),
            }
        })
        .collect()
}

fn explicit_tab_labels(snapshot: &SessionSnapshot) -> HashMap<&str, &str> {
    let mut positions = HashMap::new();
    snapshot
        .tabs
        .iter()
        .filter_map(|tab| {
            let position = positions.entry(tab.workspace_id.as_str()).or_insert(0usize);
            *position += 1;
            (!tab.label.trim().is_empty() && tab.label != position.to_string())
                .then_some((tab.tab_id.as_str(), tab.label.as_str()))
        })
        .collect()
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn agent_status_priority(status: &AgentStatus) -> u8 {
    match status.as_str() {
        AgentStatus::BLOCKED => 0,
        AgentStatus::DONE => 1,
        AgentStatus::WORKING => 2,
        AgentStatus::IDLE => 3,
        _ => 4,
    }
}

fn native_dispatch(
    action: &str,
    snapshot: &SessionSnapshot,
    context: Option<&PluginInvocationContext>,
) -> Option<Dispatch> {
    let pane_id = context
        .and_then(|context| context.focused_pane_id.as_deref())
        .or(snapshot.focused_pane_id.as_deref());
    let workspace_id = context
        .and_then(|context| context.workspace_id.as_deref())
        .or(snapshot.focused_workspace_id.as_deref());
    let cwd = context.and_then(|context| context.focused_pane_cwd.clone());

    match action {
        "reload_config" => Some(Dispatch::new("server.reload_config", json!({}))),
        "new_workspace" => Some(Dispatch::new(
            "workspace.create",
            json!({ "cwd": cwd, "focus": true }),
        )),
        "new_tab" => Some(Dispatch::new(
            "tab.create",
            json!({ "workspace_id": workspace_id, "cwd": cwd, "focus": true }),
        )),
        "split_vertical" => Some(Dispatch::new(
            "pane.split",
            json!({ "target_pane_id": pane_id?, "direction": "right", "cwd": cwd, "focus": true }),
        )),
        "split_horizontal" => Some(Dispatch::new(
            "pane.split",
            json!({ "target_pane_id": pane_id?, "direction": "down", "cwd": cwd, "focus": true }),
        )),
        "zoom" | "fullscreen" => Some(Dispatch::new(
            "pane.zoom",
            json!({ "pane_id": pane_id?, "mode": "toggle" }),
        )),
        "focus_pane_left" => pane_direction(pane_id?, "left"),
        "focus_pane_down" => pane_direction(pane_id?, "down"),
        "focus_pane_up" => pane_direction(pane_id?, "up"),
        "focus_pane_right" => pane_direction(pane_id?, "right"),
        "swap_pane_left" => pane_swap(pane_id?, "left"),
        "swap_pane_down" => pane_swap(pane_id?, "down"),
        "swap_pane_up" => pane_swap(pane_id?, "up"),
        "swap_pane_right" => pane_swap(pane_id?, "right"),
        "previous_workspace" => neighbor_workspace(snapshot, -1),
        "next_workspace" => neighbor_workspace(snapshot, 1),
        "previous_tab" => neighbor_tab(snapshot, workspace_id?, -1),
        "next_tab" => neighbor_tab(snapshot, workspace_id?, 1),
        "cycle_pane_previous" => neighbor_pane(snapshot, pane_id?, -1),
        "cycle_pane_next" => neighbor_pane(snapshot, pane_id?, 1),
        "previous_agent" => neighbor_agent(snapshot, pane_id?, -1),
        "next_agent" => neighbor_agent(snapshot, pane_id?, 1),
        _ => None,
    }
}

fn pane_direction(pane_id: &str, direction: &str) -> Option<Dispatch> {
    Some(Dispatch::new(
        "pane.focus_direction",
        json!({ "pane_id": pane_id, "direction": direction }),
    ))
}

fn pane_swap(pane_id: &str, direction: &str) -> Option<Dispatch> {
    Some(Dispatch::new(
        "pane.swap",
        json!({ "pane_id": pane_id, "direction": direction }),
    ))
}

fn neighbor_workspace(snapshot: &SessionSnapshot, delta: isize) -> Option<Dispatch> {
    let mut values: Vec<_> = snapshot.workspaces.iter().collect();
    values.sort_by_key(|workspace| workspace.number);
    let current = snapshot.focused_workspace_id.as_deref()?;
    let index = values
        .iter()
        .position(|workspace| workspace.workspace_id == current)?;
    let target = values[neighbor_index(index, values.len(), delta)];
    Some(Dispatch::new(
        "workspace.focus",
        json!({ "workspace_id": target.workspace_id }),
    ))
}

fn neighbor_tab(snapshot: &SessionSnapshot, workspace_id: &str, delta: isize) -> Option<Dispatch> {
    let mut values: Vec<_> = snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == workspace_id)
        .collect();
    values.sort_by_key(|tab| tab.number);
    let current = snapshot.focused_tab_id.as_deref()?;
    let index = values.iter().position(|tab| tab.tab_id == current)?;
    let target = values[neighbor_index(index, values.len(), delta)];
    Some(Dispatch::new(
        "tab.focus",
        json!({ "tab_id": target.tab_id }),
    ))
}

fn neighbor_pane(snapshot: &SessionSnapshot, pane_id: &str, delta: isize) -> Option<Dispatch> {
    let tab_id = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)?
        .tab_id
        .as_str();
    let values: Vec<_> = snapshot
        .panes
        .iter()
        .filter(|pane| pane.tab_id == tab_id)
        .collect();
    let index = values.iter().position(|pane| pane.pane_id == pane_id)?;
    let target = values[neighbor_index(index, values.len(), delta)];
    Some(Dispatch::new(
        "pane.focus",
        json!({ "pane_id": target.pane_id }),
    ))
}

fn neighbor_agent(snapshot: &SessionSnapshot, pane_id: &str, delta: isize) -> Option<Dispatch> {
    let index = snapshot
        .agents
        .iter()
        .position(|agent| agent.pane_id == pane_id)?;
    let target = &snapshot.agents[neighbor_index(index, snapshot.agents.len(), delta)];
    Some(Dispatch::new(
        "agent.focus",
        json!({ "target": target.pane_id }),
    ))
}

fn neighbor_index(index: usize, len: usize, delta: isize) -> usize {
    (index as isize + delta).rem_euclid(len.max(1) as isize) as usize
}

fn invocation_context() -> Option<PluginInvocationContext> {
    std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn load_native_bindings() -> Result<Vec<NativeBinding>, String> {
    let herdr = std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into());
    let output = Command::new(herdr)
        .arg("--default-config")
        .output()
        .map_err(|error| format!("could not run herdr --default-config: {error}"))?;
    if !output.status.success() {
        return Err("herdr --default-config failed".into());
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|error| format!("default config is not UTF-8: {error}"))?;
    let mut bindings = parse_default_bindings(&text);
    for action in SAFE_API_ACTIONS {
        if !bindings.iter().any(|binding| binding.action == action) {
            bindings.push(NativeBinding {
                action: action.into(),
                keys: Vec::new(),
            });
        }
    }

    if let Some(table) = host_keys_table() {
        for binding in &mut bindings {
            if let Some(keys) = table.get(&binding.action).and_then(parse_keys) {
                binding.keys = keys;
            }
        }
    }
    Ok(bindings)
}

fn parse_default_bindings(text: &str) -> Vec<NativeBinding> {
    let mut bindings = Vec::new();
    let mut in_keys = false;
    for raw in text.lines() {
        let line = raw.trim_start();
        let uncommented = line.strip_prefix('#').map(str::trim).unwrap_or(line);
        if uncommented.starts_with('[') {
            in_keys = uncommented == "[keys]";
            continue;
        }
        if !in_keys {
            continue;
        }
        let Some(line) = line.strip_prefix('#').map(str::trim) else {
            continue;
        };
        let Some((name, raw_value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name == "prefix"
            || !name
                .chars()
                .all(|character| character.is_ascii_lowercase() || character == '_')
        {
            continue;
        }
        let Ok(value) = toml::from_str::<Value>(&format!("value = {}", raw_value.trim())) else {
            continue;
        };
        let keys = value.get("value").and_then(parse_keys).unwrap_or_default();
        bindings.push(NativeBinding {
            action: name.into(),
            keys,
        });
    }
    bindings
}

fn load_plugin_keybindings() -> HashMap<String, Vec<String>> {
    let Some(table) = host_keys_table() else {
        return HashMap::new();
    };
    let Some(commands) = table.get("command").and_then(Value::as_array) else {
        return HashMap::new();
    };
    let mut result: HashMap<String, Vec<String>> = HashMap::new();
    for command in commands {
        if command.get("type").and_then(Value::as_str) != Some("plugin_action") {
            continue;
        }
        let Some(action) = command.get("command").and_then(Value::as_str) else {
            continue;
        };
        let Some(keys) = command.get("key").and_then(parse_keys) else {
            continue;
        };
        result.entry(action.into()).or_default().extend(keys);
    }
    result
}

fn host_keys_table() -> Option<Map<String, Value>> {
    let raw = std::fs::read_to_string(herdr_config_path()).ok()?;
    let value: Value = toml::from_str(&raw).ok()?;
    value.get("keys")?.as_object().cloned()
}

fn parse_keys(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::String(key) => Some((!key.is_empty()).then(|| key.clone()).into_iter().collect()),
        Value::Array(keys) => Some(
            keys.iter()
                .filter_map(Value::as_str)
                .filter(|key| !key.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        _ => None,
    }
}

fn humanize(action: &str) -> String {
    let mut label = action.replace('_', " ");
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_client::{AgentInfo, TabInfo, WorkspaceInfo};

    fn agent(pane_id: &str, status: &str, state_change_seq: u64) -> AgentInfo {
        AgentInfo {
            terminal_id: format!("terminal-{pane_id}"),
            name: Some(pane_id.into()),
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: None,
            agent_status: status.into(),
            screen_detection_skipped: false,
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            agent_session: None,
            workspace_id: "workspace-1".into(),
            tab_id: "tab-1".into(),
            pane_id: pane_id.into(),
            focused: false,
            launch_pending: false,
            interactive_ready: true,
            state_change_seq,
            cwd: None,
            foreground_cwd: None,
            revision: 0,
        }
    }

    fn snapshot(agents: Vec<AgentInfo>) -> SessionSnapshot {
        SessionSnapshot {
            version: "0.7.5".into(),
            protocol: 17,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            layouts: Vec::new(),
            agents,
        }
    }

    fn snapshot_with_tab(info: AgentInfo, label: &str, number: usize) -> SessionSnapshot {
        let mut snapshot = snapshot(vec![info]);
        snapshot.workspaces.push(WorkspaceInfo {
            workspace_id: "workspace-1".into(),
            number: 1,
            label: "project".into(),
            focused: false,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: "tab-1".into(),
            agent_status: "idle".into(),
            tokens: HashMap::new(),
            worktree: None,
        });
        snapshot.tabs.push(TabInfo {
            tab_id: "tab-1".into(),
            workspace_id: "workspace-1".into(),
            number,
            label: label.into(),
            focused: false,
            pane_count: 1,
            agent_status: "idle".into(),
        });
        snapshot
    }

    #[test]
    fn default_config_discovery_keeps_empty_and_multiple_bindings() {
        let parsed = parse_default_bindings(
            r#"
[theme]
# name = "terminal"
[keys]
# prefix = "ctrl+b"
# help = "prefix+?"
# focus_agent = ""
# split_vertical = ["prefix+v", "ctrl+alt+v"]
# [[keys.command]]
# key = "prefix+x"
# type = "shell"
# command = "echo nope"
"#,
        );
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].action, "help");
        assert!(parsed[1].keys.is_empty());
        assert_eq!(parsed[2].keys, ["prefix+v", "ctrl+alt+v"]);
    }

    #[test]
    fn safe_api_actions_dispatch_without_destructive_closes() {
        let snapshot = SessionSnapshot {
            version: "0.7.5".into(),
            protocol: 17,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: Some("pane-1".into()),
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            layouts: Vec::new(),
            agents: Vec::new(),
        };

        let dispatch = native_dispatch("swap_pane_left", &snapshot, None).unwrap();
        assert_eq!(dispatch.method, "pane.swap");
        assert_eq!(
            dispatch.params,
            json!({ "pane_id": "pane-1", "direction": "left" })
        );
        assert!(native_dispatch("close_pane", &snapshot, None).is_none());
    }

    #[test]
    fn agents_prioritize_attention_and_recency_and_focus_panes() {
        let snapshot = snapshot(vec![
            agent("idle", "idle", 1),
            agent("blocked-old", "blocked", 2),
            agent("unknown", "future", 99),
            agent("working", "working", 4),
            agent("done", "done", 3),
            agent("blocked-new", "blocked", 5),
        ]);

        let items = agents(&snapshot);
        assert_eq!(
            items
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            [
                "workspace-1: blocked-new",
                "workspace-1: blocked-old",
                "workspace-1: done",
                "workspace-1: working",
                "workspace-1: idle",
                "workspace-1: unknown"
            ]
        );
        assert_eq!(items[0].dispatch.params, json!({ "target": "blocked-new" }));

        let dispatch = neighbor_agent(&snapshot, "blocked-old", 1).unwrap();
        assert_eq!(dispatch.params, json!({ "target": "unknown" }));
    }

    #[test]
    fn agent_titles_prefix_the_workspace_and_prefer_name_then_tab_then_terminal_then_id() {
        let mut info = agent("pane-1", "idle", 1);
        info.name = Some("reviewer".into());
        info.agent = Some("codex".into());
        info.terminal_title_stripped = Some("Fix login".into());
        assert_eq!(
            agents(&snapshot_with_tab(info, "agents", 99))[0].title,
            "project: reviewer"
        );

        let mut info = agent("pane-1", "idle", 1);
        info.name = None;
        info.terminal_title_stripped = Some("Fix login".into());
        assert_eq!(
            agents(&snapshot_with_tab(info, "agents", 99))[0].title,
            "project: agents"
        );

        let mut info = agent("pane-1", "idle", 1);
        info.name = None;
        info.terminal_title_stripped = Some("Fix login".into());
        assert_eq!(
            agents(&snapshot_with_tab(info, "1", 99))[0].title,
            "project: Fix login"
        );

        let mut info = agent("pane-1", "idle", 1);
        info.name = None;
        info.terminal_title_stripped = Some("Fix login".into());
        assert_eq!(
            agents(&snapshot_with_tab(info, "   ", 99))[0].title,
            "project: Fix login"
        );

        let mut info = agent("pane-1", "idle", 1);
        info.name = None;
        info.terminal_title = Some("Raw terminal title".into());
        assert_eq!(
            agents(&snapshot_with_tab(info, "1", 99))[0].title,
            "project: Raw terminal title"
        );

        let mut info = agent("pane-1", "idle", 1);
        info.name = None;
        assert_eq!(
            agents(&snapshot_with_tab(info, "1", 99))[0].title,
            "project: terminal-pane-1"
        );
    }

    #[test]
    fn agent_titles_skip_workspace_equal_candidates() {
        let mut info = agent("pane-1", "idle", 1);
        info.name = Some("project".into());
        info.terminal_title_stripped = Some("project".into());
        info.terminal_title = Some("Fix login".into());

        assert_eq!(
            agents(&snapshot_with_tab(info, "project", 99))[0].title,
            "project: Fix login"
        );

        let mut info = agent("pane-1", "idle", 1);
        info.name = Some("project".into());
        info.terminal_title_stripped = Some("project".into());
        info.terminal_title = Some("project".into());
        assert_eq!(
            agents(&snapshot_with_tab(info, "project", 99))[0].title,
            "project: terminal-pane-1"
        );

        let mut info = agent("pane-1", "idle", 1);
        info.terminal_id = "project".into();
        info.name = Some("project".into());
        info.terminal_title_stripped = Some("project".into());
        info.terminal_title = Some("project".into());
        assert_eq!(
            agents(&snapshot_with_tab(info, "project", 99))[0].title,
            "project"
        );
    }

    #[test]
    fn agents_put_path_and_kind_in_subtitle() {
        let mut info = agent("pane-1", "blocked", 1);
        info.name = None;
        info.agent = Some("codex".into());
        info.title = Some("Review the diff".into());
        info.cwd = Some("/project".into());
        info.foreground_cwd = Some("/foreground-project".into());
        let item = agents(&snapshot_with_tab(info, "agents", 1)).pop().unwrap();

        assert_eq!(item.title, "project: agents");
        assert_eq!(item.subtitle, "/foreground-project · codex");
        assert_eq!(item.agent_status, Some("blocked".into()));
        assert!(item.detail.contains("Review the diff"));

        let mut info = agent("pane-1", "blocked", 1);
        info.name = None;
        info.agent = Some("codex".into());
        info.cwd = Some("/project".into());
        info.foreground_cwd = Some("  ".into());
        let item = agents(&snapshot_with_tab(info, "agents", 1)).pop().unwrap();
        assert_eq!(item.subtitle, "/project · codex");

        let mut info = agent("pane-1", "blocked", 1);
        info.name = None;
        info.agent = Some("codex".into());
        let item = agents(&snapshot_with_tab(info, "agents", 1)).pop().unwrap();
        assert_eq!(item.subtitle, "codex");

        let mut info = agent("pane-1", "blocked", 1);
        info.name = None;
        info.cwd = Some("/project".into());
        let item = agents(&snapshot_with_tab(info, "agents", 1)).pop().unwrap();
        assert_eq!(item.subtitle, "/project");

        let mut info = agent("pane-1", "blocked", 1);
        info.name = None;
        let item = agents(&snapshot_with_tab(info, "agents", 1)).pop().unwrap();
        assert!(item.subtitle.is_empty());
    }
}
