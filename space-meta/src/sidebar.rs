// Pure sidebar computations: stable display order and token formatting.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use herdr_client::{SessionSnapshot, WorkspaceInfo};

const BRAILLE_BLANK: char = '\u{2800}';

pub struct DisplayEntry<'a> {
    pub workspace: &'a WorkspaceInfo,
    pub grouped_child: bool,
}

/// Stable sidebar order with worktree groups expanded. A grouped repo's
/// non-linked parent is emitted at the position of its first member, with the
/// linked-worktree children right after it. Herdr does not expose desktop group
/// collapse state through its plugin API.
pub fn display_order(snapshot: &SessionSnapshot) -> Vec<DisplayEntry<'_>> {
    let mut members_by_key = HashMap::<&str, Vec<usize>>::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        if let Some(worktree) = &workspace.worktree {
            members_by_key
                .entry(&worktree.repo_key)
                .or_default()
                .push(index);
        }
    }
    let grouped_parents: HashMap<&str, usize> = members_by_key
        .iter()
        .filter(|(_, members)| members.len() >= 2)
        .filter_map(|(key, members)| {
            members
                .iter()
                .copied()
                .find(|index| {
                    snapshot.workspaces[*index]
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| !worktree.is_linked_worktree)
                })
                .map(|parent| (*key, parent))
        })
        .collect();

    let mut emitted_groups = HashSet::<&str>::new();
    let mut ordered = Vec::with_capacity(snapshot.workspaces.len());
    for workspace in &snapshot.workspaces {
        let Some((key, parent)) = workspace
            .worktree
            .as_ref()
            .map(|worktree| worktree.repo_key.as_str())
            .and_then(|key| grouped_parents.get(key).map(|&parent| (key, parent)))
        else {
            ordered.push(DisplayEntry {
                workspace,
                grouped_child: false,
            });
            continue;
        };
        if !emitted_groups.insert(key) {
            continue;
        }
        let members = &members_by_key[key];
        ordered.push(DisplayEntry {
            workspace: &snapshot.workspaces[parent],
            grouped_child: false,
        });
        ordered.extend(
            members
                .iter()
                .copied()
                .filter(|member| *member != parent)
                .map(|member| DisplayEntry {
                    workspace: &snapshot.workspaces[member],
                    grouped_child: true,
                }),
        );
    }
    ordered
}

/// The three tokens published per workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub numbered_workspace: String,
    pub branch_line: Option<String>,
    pub git_dirty: Option<String>,
}

/// Composite number/name first row for every space, plus an aligned branch
/// line for top-level spaces. Grouped worktree children use their branch as
/// the first-row label. Herdr's public plugin API does not expose whether a
/// child has a custom name, so that case cannot exactly match the built-in row.
pub fn format_tokens(
    num: usize,
    workspace_label: &str,
    branch: Option<&str>,
    pr: Option<&str>,
    dirty: bool,
    grouped_child: bool,
) -> Tokens {
    let num = num.to_string();
    let git_dirty = dirty.then(|| "\u{f448}".to_owned());
    let pr_suffix = pr.map_or_else(String::new, |number| format!(" #{number}"));
    if grouped_child {
        let label = branch
            .map(|branch| branch.strip_prefix("worktree/").unwrap_or(branch))
            .unwrap_or(workspace_label);
        return Tokens {
            numbered_workspace: format!("{num} {label}{pr_suffix}"),
            branch_line: None,
            git_dirty,
        };
    }

    // Herdr prefixes continuation rows by 3 cells. The first row begins with
    // 1 cell, then `state_icon ` and `$numbered_workspace` begins. Within the
    // composite token, the label follows `num `. Padding by digit-count + 1
    // therefore puts the branch under that label exactly.
    let branch_line = branch.map(|branch| {
        let padding = BRAILLE_BLANK.to_string().repeat(num.chars().count() + 1);
        format!("{padding}{branch}{pr_suffix}")
    });
    Tokens {
        numbered_workspace: format!("{num} {workspace_label}"),
        branch_line,
        git_dirty,
    }
}

/// The checkout a workspace's git metadata comes from: its worktree checkout
/// when Herdr manages one, otherwise the first pane of its first tab.
pub fn workspace_cwd(snapshot: &SessionSnapshot, workspace: &WorkspaceInfo) -> Option<PathBuf> {
    if let Some(worktree) = &workspace.worktree {
        return Some(PathBuf::from(&worktree.checkout_path));
    }
    let tab = snapshot
        .tabs
        .iter()
        .find(|tab| tab.workspace_id == workspace.workspace_id)?;
    snapshot
        .panes
        .iter()
        .find(|pane| pane.tab_id == tab.tab_id)
        .and_then(|pane| pane.cwd.as_deref())
        .map(PathBuf::from)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use herdr_client::{AgentStatus, WorkspaceWorktreeInfo};
    use serde_json::json;

    pub(crate) fn snapshot(workspaces: Vec<WorkspaceInfo>) -> SessionSnapshot {
        SessionSnapshot {
            version: "test".into(),
            protocol: 0,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            client_focused: None,
            workspaces,
            tabs: Vec::new(),
            panes: Vec::new(),
            layouts: Vec::new(),
            agents: Vec::new(),
        }
    }

    pub(crate) fn workspace(id: &str, group: Option<(&str, bool)>) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: id.into(),
            number: 0,
            label: id.into(),
            focused: false,
            pane_count: 0,
            tab_count: 0,
            active_tab_id: String::new(),
            agent_status: AgentStatus::from(AgentStatus::UNKNOWN),
            tokens: HashMap::new(),
            worktree: group.map(|(key, linked)| WorkspaceWorktreeInfo {
                repo_key: key.into(),
                repo_name: key.into(),
                repo_root: format!("/{key}"),
                checkout_path: format!("/{id}"),
                is_linked_worktree: linked,
            }),
        }
    }

    fn display_roles(snapshot: &SessionSnapshot) -> Vec<(&str, bool)> {
        display_order(snapshot)
            .into_iter()
            .map(|entry| (entry.workspace.workspace_id.as_str(), entry.grouped_child))
            .collect()
    }

    #[test]
    fn display_order_preserves_plain_workspace_order() {
        let snapshot = snapshot(vec![
            workspace("first", None),
            workspace("second", None),
            workspace("third", None),
        ]);
        assert_eq!(
            display_roles(&snapshot),
            [("first", false), ("second", false), ("third", false)]
        );
    }

    #[test]
    fn display_order_emits_noncontiguous_group_at_its_first_member() {
        let snapshot = snapshot(vec![
            workspace("child-a", Some(("repo", true))),
            workspace("plain", None),
            workspace("parent", Some(("repo", false))),
            workspace("child-b", Some(("repo", true))),
        ]);
        assert_eq!(
            display_roles(&snapshot),
            [
                ("parent", false),
                ("child-a", true),
                ("child-b", true),
                ("plain", false),
            ]
        );
    }

    #[test]
    fn display_order_leaves_linked_only_and_singleton_sets_ungrouped() {
        let snapshot = snapshot(vec![
            workspace("linked-a", Some(("linked-only", true))),
            workspace("single-parent", Some(("singleton", false))),
            workspace("plain", None),
            workspace("linked-b", Some(("linked-only", true))),
        ]);
        assert_eq!(
            display_roles(&snapshot),
            [
                ("linked-a", false),
                ("single-parent", false),
                ("plain", false),
                ("linked-b", false),
            ]
        );
    }

    #[test]
    fn workspace_cwd_uses_worktree_or_first_pane_cwd() {
        let mut snapshot = snapshot(vec![workspace("workspace", None)]);
        snapshot.tabs = serde_json::from_value(json!([{
            "tab_id": "first-tab", "workspace_id": "workspace", "number": 1,
            "label": "first", "focused": true, "pane_count": 2, "agent_status": "unknown"
        }]))
        .unwrap();
        snapshot.panes = serde_json::from_value(json!([
            {
                "pane_id": "first-pane", "terminal_id": "first-terminal",
                "workspace_id": "workspace", "tab_id": "first-tab", "focused": false,
                "cwd": "/first-pane", "foreground_cwd": "/first-pane/nested/other-repo",
                "agent_status": "unknown", "revision": 0
            },
            {
                "pane_id": "second-pane", "terminal_id": "second-terminal",
                "workspace_id": "workspace", "tab_id": "first-tab", "focused": false,
                "cwd": "/second-pane", "agent_status": "unknown", "revision": 0
            }
        ]))
        .unwrap();

        // Without worktree metadata, use the first pane's CWD, not its foreground CWD.
        assert_eq!(
            workspace_cwd(&snapshot, &snapshot.workspaces[0]),
            Some(PathBuf::from("/first-pane"))
        );
        snapshot.workspaces[0].worktree = workspace("workspace", Some(("repo", true))).worktree;
        assert_eq!(
            workspace_cwd(&snapshot, &snapshot.workspaces[0]),
            Some(PathBuf::from("/workspace"))
        );
        snapshot.workspaces[0].worktree = None;
        snapshot.panes[0].cwd = None;
        assert_eq!(workspace_cwd(&snapshot, &snapshot.workspaces[0]), None);
    }

    #[test]
    fn top_level_tokens_align_branch_under_composite_name() {
        let one_digit = format_tokens(7, "project", Some("main"), Some("123"), true, false);
        assert_eq!(one_digit.numbered_workspace, "7 project");
        assert_eq!(
            one_digit.branch_line.as_deref(),
            Some(format!("{}main #123", BRAILLE_BLANK.to_string().repeat(2)).as_str())
        );
        assert_eq!(one_digit.git_dirty.as_deref(), Some("\u{f448}"));

        let two_digits = format_tokens(16, "project", Some("main"), None, false, false);
        assert_eq!(two_digits.numbered_workspace, "16 project");
        assert_eq!(
            two_digits.branch_line.as_deref(),
            Some(format!("{}main", BRAILLE_BLANK.to_string().repeat(3)).as_str())
        );
        assert_eq!(two_digits.git_dirty, None);
    }

    #[test]
    fn grouped_child_folds_branch_and_pr_into_first_row() {
        let child = format_tokens(
            4,
            "custom-label",
            Some("worktree/feature"),
            Some("88"),
            false,
            true,
        );
        assert_eq!(child.numbered_workspace, "4 feature #88");
        assert_eq!(child.branch_line, None);

        let detached = format_tokens(4, "fallback-label", None, None, false, true);
        assert_eq!(detached.numbered_workspace, "4 fallback-label");
    }
}
