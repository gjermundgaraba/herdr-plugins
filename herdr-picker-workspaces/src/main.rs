//! Live workspace source: streams workspace items to herdr-picker.

use std::process::ExitCode;

use herdr_client::SessionSnapshot;
use herdr_picker_sdk::{Item, presentation, run, serve};
use serde_json::json;

fn main() -> ExitCode {
    run(env!("CARGO_BIN_NAME"), serve(workspace_items))
}

fn workspace_items(snapshot: &SessionSnapshot) -> Vec<Item> {
    let mut workspaces = snapshot.workspaces.iter().collect::<Vec<_>>();
    workspaces.sort_by_key(|workspace| workspace.number);
    workspaces
        .into_iter()
        .map(|workspace| {
            let (indicator, tone, spinning) = presentation(&workspace.agent_status);
            Item {
                id: workspace.workspace_id.clone(),
                title: workspace.label.clone(),
                subtitle: format!(
                    "{} tabs · {} panes · {}",
                    workspace.tab_count, workspace.pane_count, workspace.agent_status
                ),
                detail: workspace
                    .worktree
                    .as_ref()
                    .map(|worktree| {
                        format!("{} · {}", workspace.workspace_id, worktree.checkout_path)
                    })
                    .unwrap_or_else(|| workspace.workspace_id.clone()),
                badge: workspace.number.to_string(),
                indicator: indicator.into(),
                tone: Some(tone),
                spinning,
                search: String::new(),
                value: json!({
                    "workspace_id": workspace.workspace_id,
                    "number": workspace.number,
                }),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_client::WorkspaceInfo;
    use std::collections::HashMap;

    fn workspace(workspace_id: &str, number: usize, label: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: workspace_id.into(),
            number,
            label: label.into(),
            focused: false,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: "t1".into(),
            agent_status: "idle".into(),
            tokens: HashMap::new(),
            worktree: None,
        }
    }

    #[test]
    fn workspace_picker_preserves_workspace_order_and_focus_value() {
        let snapshot = SessionSnapshot {
            version: "0.8.0".into(),
            protocol: 19,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            client_focused: None,
            workspaces: vec![workspace("w2", 2, "second"), workspace("w1", 1, "project")],
            tabs: Vec::new(),
            panes: Vec::new(),
            layouts: Vec::new(),
            agents: Vec::new(),
        };

        let items = workspace_items(&snapshot);
        assert_eq!(items[0].id, "w1");
        assert_eq!(items[1].value["workspace_id"], "w2");
    }
}
