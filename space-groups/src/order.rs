// Worktree families and the server order, mirrored from the fork's sidebar:
// a repo with an open checkout and at least one other space forms a family
// that shares the checkout's group, and groups sit above ungrouped spaces.
use herdr_client::WorkspaceInfo;

use crate::TOKEN_KEY;

/// The space whose group a family shares: the first non-linked checkout of a
/// repo with at least two open spaces. A space outside a family is its own anchor.
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

/// Every space that shares `anchor`'s group, anchor first, then server order.
pub fn family<'a>(workspaces: &'a [WorkspaceInfo], anchor_id: &'a str) -> Vec<&'a str> {
    let mut family = vec![anchor_id];
    family.extend(
        workspaces
            .iter()
            .filter(|workspace| workspace.workspace_id != anchor_id)
            .filter(|workspace| anchor(workspaces, workspace).workspace_id == anchor_id)
            .map(|workspace| workspace.workspace_id.as_str()),
    );
    family
}

fn group_of<'a>(workspaces: &'a [WorkspaceInfo], workspace: &'a WorkspaceInfo) -> Option<&'a str> {
    anchor(workspaces, workspace)
        .tokens
        .get(TOKEN_KEY)
        .map(String::as_str)
        .filter(|group| !group.is_empty())
}

/// Where `family` goes when it moves into `group`: right after the group's
/// last member, or, for a new group, above the first ungrouped space. Leaving
/// every group moves the family to the end. The result is the space to move
/// the family before, `None` for the end; the outer `None` means it is
/// already in place.
pub fn placement(
    workspaces: &[WorkspaceInfo],
    family: &[&str],
    group: Option<&str>,
) -> Option<Option<String>> {
    let rest = workspaces
        .iter()
        .filter(|workspace| !family.contains(&workspace.workspace_id.as_str()))
        .collect::<Vec<_>>();
    let at = match group {
        None => rest.len(),
        Some(group) => rest
            .iter()
            .rposition(|workspace| group_of(workspaces, workspace) == Some(group))
            .map(|last| last + 1)
            .or_else(|| {
                rest.iter()
                    .position(|workspace| group_of(workspaces, workspace).is_none())
            })
            .unwrap_or(rest.len()),
    };
    let mut ids = rest
        .iter()
        .map(|workspace| workspace.workspace_id.as_str())
        .collect::<Vec<_>>();
    ids.splice(at..at, family.iter().copied());
    let unchanged = workspaces
        .iter()
        .map(|workspace| workspace.workspace_id.as_str())
        .eq(ids);
    (!unchanged).then(|| rest.get(at).map(|workspace| workspace.workspace_id.clone()))
}

/// The group the family of `anchor_id` takes after a move left it where it
/// now sits, when that differs from its current group. Landing inside a run
/// joins that run's group, or leaves every group inside the ungrouped spaces.
/// At a run boundary it keeps its group if a neighbour shares it or it is the
/// group's only family, and otherwise joins the run above it.
pub fn regroup(workspaces: &[WorkspaceInfo], anchor_id: &str) -> Option<Option<String>> {
    // Top-level units in sidebar order: a family shows where its first member is.
    let mut anchors = Vec::<&WorkspaceInfo>::new();
    for workspace in workspaces {
        let anchor = anchor(workspaces, workspace);
        if !anchors
            .iter()
            .any(|seen| seen.workspace_id == anchor.workspace_id)
        {
            anchors.push(anchor);
        }
    }
    let index = anchors
        .iter()
        .position(|anchor| anchor.workspace_id == anchor_id)?;
    let groups = anchors
        .iter()
        .map(|anchor| group_of(workspaces, anchor))
        .collect::<Vec<_>>();
    let current = groups[index];
    let previous = index.checked_sub(1).map(|index| groups[index]);
    let next = groups.get(index + 1).copied();
    let only_family =
        current.is_some() && groups.iter().filter(|group| **group == current).count() == 1;
    let target = match (previous, next) {
        (Some(previous), Some(next)) if previous == next => previous,
        _ if previous.flatten() == current || next.flatten() == current || only_family => current,
        _ => previous.unwrap_or_else(|| next.flatten()),
    };
    (target != current).then(|| target.map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn space(id: &str, group: Option<&str>, repo: Option<(&str, bool)>) -> WorkspaceInfo {
        serde_json::from_value(json!({
            "workspace_id": id, "number": 1, "label": id, "focused": false,
            "pane_count": 1, "tab_count": 1, "active_tab_id": format!("{id}:t1"),
            "agent_status": "idle",
            "tokens": group.map_or_else(|| json!({}), |group| json!({"space_group": group})),
            "worktree": repo.map(|(key, linked)| json!({
                "repo_key": key, "repo_name": key, "repo_root": format!("/{key}"),
                "checkout_path": format!("/{id}"), "is_linked_worktree": linked,
            })),
        }))
        .unwrap()
    }

    #[test]
    fn a_linked_child_resolves_to_its_checkout_family() {
        let spaces = [
            space("child", None, Some(("repo", true))),
            space("plain", None, None),
            space("parent", None, Some(("repo", false))),
            space("lonely", None, Some(("solo", true))),
        ];
        assert_eq!(anchor(&spaces, &spaces[0]).workspace_id, "parent");
        assert_eq!(anchor(&spaces, &spaces[3]).workspace_id, "lonely");
        assert_eq!(family(&spaces, "parent"), ["parent", "child"]);
        assert_eq!(family(&spaces, "plain"), ["plain"]);
    }

    #[test]
    fn joining_a_group_lands_after_its_last_member() {
        let spaces = [
            space("a1", Some("a"), None),
            space("b1", Some("b"), None),
            space("u1", None, None),
            space("x", None, None),
        ];
        assert_eq!(
            placement(&spaces, &["x"], Some("a")),
            Some(Some("b1".into()))
        );
    }

    #[test]
    fn a_new_group_goes_above_the_ungrouped_spaces() {
        let spaces = [
            space("a1", Some("a"), None),
            space("u1", None, None),
            space("x", None, None),
        ];
        assert_eq!(
            placement(&spaces, &["x"], Some("new")),
            Some(Some("u1".into()))
        );
    }

    #[test]
    fn leaving_moves_the_family_to_the_end_and_unchanged_is_none() {
        let spaces = [
            space("parent", Some("a"), Some(("repo", false))),
            space("child", None, Some(("repo", true))),
            space("u1", None, None),
        ];
        assert_eq!(placement(&spaces, &["parent", "child"], None), Some(None));
        assert_eq!(placement(&spaces, &["parent", "child"], Some("a")), None);
    }

    fn regrouped(order: &[(&str, Option<&str>)], moved: &str) -> Option<Option<String>> {
        let spaces = order
            .iter()
            .map(|(id, group)| space(id, *group, None))
            .collect::<Vec<_>>();
        regroup(&spaces, moved)
    }

    #[test]
    fn landing_inside_a_run_joins_its_group() {
        let a = Some("a");
        assert_eq!(
            regrouped(
                &[("a1", a), ("x", Some("b")), ("a2", a), ("b1", Some("b"))],
                "x"
            ),
            Some(Some("a".into()))
        );
        assert_eq!(
            regrouped(&[("a1", a), ("u1", None), ("x", a), ("u2", None)], "x"),
            Some(None)
        );
        assert_eq!(
            regrouped(&[("a1", a), ("x", None), ("a2", a)], "x"),
            Some(a.map(Into::into))
        );
    }

    #[test]
    fn a_boundary_keeps_a_shared_group_and_otherwise_joins_the_run_above() {
        let (a, b) = (Some("a"), Some("b"));
        // The end of its own run, or the start of the ungrouped spaces.
        assert_eq!(regrouped(&[("a1", a), ("x", a), ("b1", b)], "x"), None);
        assert_eq!(
            regrouped(&[("a1", a), ("x", None), ("u1", None)], "x"),
            None
        );
        // Separated from its group between two others: the run above wins.
        assert_eq!(
            regrouped(
                &[("a1", a), ("x", Some("c")), ("b1", b), ("c1", Some("c"))],
                "x"
            ),
            Some(a.map(Into::into))
        );
        // At the top there is no run above, so it joins the run below.
        assert_eq!(
            regrouped(&[("x", Some("c")), ("a1", a), ("c1", Some("c"))], "x"),
            Some(a.map(Into::into))
        );
    }

    #[test]
    fn a_group_of_one_family_moves_whole() {
        let (a, b) = (Some("a"), Some("b"));
        assert_eq!(
            regrouped(&[("a1", a), ("x", Some("solo")), ("b1", b)], "x"),
            None
        );
        assert_eq!(regrouped(&[("u1", None), ("x", Some("solo"))], "x"), None);
    }

    #[test]
    fn a_whole_group_dropped_inside_another_stays_together() {
        let (a, b) = (Some("a"), Some("b"));
        let order = [("b1", b), ("a1", a), ("a2", a), ("b2", b)];
        assert_eq!(regrouped(&order, "a1"), None);
        assert_eq!(regrouped(&order, "a2"), None);
    }

    #[test]
    fn the_placement_it_makes_itself_is_stable() {
        let a = Some("a");
        // Joining lands after the last member; leaving lands at the end.
        assert_eq!(regrouped(&[("a1", a), ("x", a), ("u1", None)], "x"), None);
        assert_eq!(
            regrouped(&[("a1", a), ("b1", Some("b")), ("x", None)], "x"),
            None
        );
    }

    #[test]
    fn a_family_is_judged_by_its_checkout() {
        let spaces = [
            space("a1", Some("a"), None),
            space("parent", None, Some(("repo", false))),
            space("a2", Some("a"), None),
            space("child", None, Some(("repo", true))),
        ];
        assert_eq!(regroup(&spaces, "parent"), Some(Some("a".into())));
    }
}
