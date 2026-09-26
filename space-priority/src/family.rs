// Worktree families, mirrored from the fork's sidebar and space-groups: a repo
// with an open checkout and at least one other space forms a family that
// shares the checkout's priority.
use herdr_client::WorkspaceInfo;

/// The space whose priority a family shares: the first non-linked checkout of
/// a repo with at least two open spaces. A space outside a family is its own anchor.
pub fn anchor<'a>(
    workspaces: &'a [WorkspaceInfo],
    workspace: &'a WorkspaceInfo,
) -> &'a WorkspaceInfo {
    let Some(worktree) = &workspace.worktree else {
        return workspace;
    };
    let members = workspaces
        .iter()
        .filter(|candidate| {
            candidate
                .worktree
                .as_ref()
                .is_some_and(|other| other.repo_key == worktree.repo_key)
        })
        .collect::<Vec<_>>();
    if members.len() < 2 {
        return workspace;
    }
    members
        .into_iter()
        .find(|member| {
            member
                .worktree
                .as_ref()
                .is_some_and(|tree| !tree.is_linked_worktree)
        })
        .unwrap_or(workspace)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;

    pub fn space(id: &str, marked: bool, repo: Option<(&str, bool)>) -> WorkspaceInfo {
        serde_json::from_value(json!({
            "workspace_id": id, "number": 1, "label": id, "focused": false,
            "pane_count": 1, "tab_count": 1, "active_tab_id": format!("{id}:t1"),
            "agent_status": "idle",
            "tokens": if marked { json!({crate::TOKEN_KEY: crate::MARK}) } else { json!({}) },
            "worktree": repo.map(|(key, linked)| json!({
                "repo_key": key, "repo_name": key, "repo_root": format!("/{key}"),
                "checkout_path": format!("/{id}"), "is_linked_worktree": linked,
            })),
        }))
        .unwrap()
    }

    #[test]
    fn a_linked_child_resolves_to_its_checkout() {
        let spaces = [
            space("child", false, Some(("repo", true))),
            space("plain", false, None),
            space("parent", false, Some(("repo", false))),
            space("lonely", false, Some(("solo", true))),
        ];
        assert_eq!(anchor(&spaces, &spaces[0]).workspace_id, "parent");
        assert_eq!(anchor(&spaces, &spaces[1]).workspace_id, "plain");
        assert_eq!(anchor(&spaces, &spaces[3]).workspace_id, "lonely");
    }
}
