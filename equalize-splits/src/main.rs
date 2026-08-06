use std::fs::File;
use std::time::Duration;

use herdr_client::{
    Client, Environment, LayoutExportParams, LayoutNode, LayoutSetSplitRatioParams, SplitDirection,
};

const SOCKET_TIMEOUT: Duration = Duration::from_secs(1);
// ponytail: Herdr layout.apply caps layouts at 24 panes; this also bounds
// retries if another client continuously changes the same layout.
const MAX_RATIO_UPDATES: usize = 64;

#[derive(Debug, Clone, PartialEq)]
struct RatioUpdate {
    path: Vec<bool>,
    ratio: f64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("equalize-splits: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let environment = Environment::load().map_err(|error| error.to_string())?;
    let Some(event) = environment.event else {
        return Ok(());
    };

    // Dispatch on HERDR_PLUGIN_EVENT: the envelope's own `event` field uses
    // underscore names (`pane_created`), not the manifest's dotted names.
    let created_pane_id = match environment.event_name.as_deref() {
        Some("pane.created") => match event.data["pane"]["pane_id"].as_str() {
            Some(pane_id) if !pane_id.is_empty() => Some(pane_id.to_owned()),
            _ => return Ok(()),
        },
        Some("pane.closed") | Some("pane.exited") => None,
        _ => return Ok(()),
    };

    let state_dir = environment
        .plugin_state_dir
        .as_ref()
        .ok_or("HERDR_PLUGIN_STATE_DIR is not set")?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state_dir.join("equalize.lock"))
        .map_err(|error| format!("open lock: {error}"))?;
    lock.lock().map_err(|error| format!("acquire lock: {error}"))?;

    let client = Client::from_env()
        .map_err(|error| error.to_string())?
        .with_timeout(SOCKET_TIMEOUT);

    let params = match &created_pane_id {
        Some(pane_id) => LayoutExportParams {
            pane_id: Some(pane_id.clone()),
            tab_id: None,
        },
        None => {
            // ponytail: pane lifecycle events lack a tab id; UI closes target the
            // workspace's active tab. Cache layouts if background API closes need support.
            let workspaces = client
                .workspaces()
                .map_err(|error| format!("list workspaces: {error}"))?;
            let Some(tab_id) = workspaces
                .into_iter()
                .find(|workspace| {
                    Some(&workspace.workspace_id) == environment.workspace_id.as_ref()
                })
                .map(|workspace| workspace.active_tab_id)
                .filter(|tab_id| !tab_id.is_empty())
            else {
                return Ok(()); // Closing the last pane may also close its workspace.
            };
            LayoutExportParams {
                tab_id: Some(tab_id),
                pane_id: None,
            }
        }
    };
    let mut layout = client
        .export_layout(&params)
        .map_err(|error| format!("export layout: {error}"))?;

    for _ in 0..MAX_RATIO_UPDATES {
        let plan = match &created_pane_id {
            Some(pane_id) => equalization_plan(&layout.root, pane_id),
            None => full_equalization_plan(&layout.root),
        };
        let Some(update) = plan.into_iter().next() else {
            return Ok(());
        };
        layout = client
            .set_split_ratio(&LayoutSetSplitRatioParams {
                tab_id: Some(layout.tab_id.clone()),
                pane_id: None,
                path: update.path,
                ratio: update.ratio,
            })
            .map_err(|error| format!("set split ratio: {error}"))?;
    }
    Err("layout kept changing while equalizing".into())
}

/// Equalize the connected same-direction region around a freshly created pane.
fn equalization_plan(root: &LayoutNode, pane_id: &str) -> Vec<RatioUpdate> {
    let mut path = Vec::new();
    if !find_pane_path(root, pane_id, &mut path) {
        return Vec::new();
    }
    // Root pane creation is not a split, and a `false` tail means a newer
    // split already moved this pane from its creation position.
    if path.pop() != Some(true) {
        return Vec::new();
    }

    // Widen from the pane's parent split to the outermost ancestor run of
    // splits in the same direction; `path` invariantly names a split below.
    let LayoutNode::Split(parent) = node_at(root, &path) else {
        unreachable!("a found pane's parent is a split");
    };
    let direction = parent.direction;
    while !path.is_empty() {
        let LayoutNode::Split(ancestor) = node_at(root, &path[..path.len() - 1]) else {
            unreachable!("every ancestor of a split is a split");
        };
        if ancestor.direction != direction {
            break;
        }
        path.pop();
    }

    let region = node_at(root, &path);
    let mut updates = Vec::new();
    collect_equalizations(region, direction, &mut path, &mut updates);
    updates
}

/// Equalize every directional region in the tab.
fn full_equalization_plan(root: &LayoutNode) -> Vec<RatioUpdate> {
    let mut updates = Vec::new();
    collect_regions(root, &mut Vec::new(), None, &mut updates);
    updates
}

fn collect_regions(
    node: &LayoutNode,
    path: &mut Vec<bool>,
    parent_direction: Option<SplitDirection>,
    updates: &mut Vec<RatioUpdate>,
) {
    let LayoutNode::Split(split) = node else {
        return;
    };
    if parent_direction != Some(split.direction) {
        collect_equalizations(node, split.direction, path, updates);
    }
    for (second, child) in [(false, &split.first), (true, &split.second)] {
        path.push(second);
        collect_regions(child, path, Some(split.direction), updates);
        path.pop();
    }
}

/// Extend `path` down to the pane; on a miss `path` is restored.
fn find_pane_path(node: &LayoutNode, pane_id: &str, path: &mut Vec<bool>) -> bool {
    match node {
        LayoutNode::Pane(pane) => pane.pane_id.as_deref() == Some(pane_id),
        LayoutNode::Split(split) => {
            for (second, child) in [(false, &split.first), (true, &split.second)] {
                path.push(second);
                if find_pane_path(child, pane_id, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
    }
}

/// Resolve a path produced by [`find_pane_path`] on the same tree.
fn node_at<'tree>(mut node: &'tree LayoutNode, path: &[bool]) -> &'tree LayoutNode {
    for &second in path {
        let LayoutNode::Split(split) = node else {
            unreachable!("a derived path never crosses a pane");
        };
        node = if second { &split.second } else { &split.first };
    }
    node
}

/// Post-order: deeper ratio corrections land before their ancestors, and the
/// pane count of `node` is returned so parents can derive their own ratio.
fn collect_equalizations(
    node: &LayoutNode,
    direction: SplitDirection,
    path: &mut Vec<bool>,
    updates: &mut Vec<RatioUpdate>,
) -> usize {
    let LayoutNode::Split(split) = node else {
        return 1;
    };
    if split.direction != direction {
        return 1;
    }

    path.push(false);
    let first = collect_equalizations(&split.first, direction, path, updates);
    path.pop();
    path.push(true);
    let second = collect_equalizations(&split.second, direction, path, updates);
    path.pop();

    let ratio = (first as f64 / (first + second) as f64).clamp(0.1, 0.9);
    if (split.ratio - ratio).abs() > 1e-6 {
        updates.push(RatioUpdate {
            path: path.clone(),
            ratio,
        });
    }
    first + second
}

#[cfg(test)]
mod tests {
    use herdr_client::{LayoutDescription, LayoutPane, LayoutSplit};

    use super::*;

    fn pane(id: &str) -> LayoutNode {
        LayoutNode::Pane(LayoutPane {
            pane_id: Some(id.into()),
            ..Default::default()
        })
    }

    fn split(direction: SplitDirection, first: LayoutNode, second: LayoutNode) -> LayoutNode {
        split_with_ratio(direction, first, second, 0.5)
    }

    fn split_with_ratio(
        direction: SplitDirection,
        first: LayoutNode,
        second: LayoutNode,
        ratio: f64,
    ) -> LayoutNode {
        LayoutNode::Split(LayoutSplit {
            direction,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        })
    }

    #[test]
    fn equalizes_three_side_by_side_panes() {
        let root = split(
            SplitDirection::Right,
            pane("a"),
            split_with_ratio(SplitDirection::Right, pane("b"), pane("c"), 0.4),
        );

        assert_eq!(
            equalization_plan(&root, "c"),
            vec![
                RatioUpdate {
                    path: vec![true],
                    ratio: 1.0 / 2.0
                },
                RatioUpdate {
                    path: vec![],
                    ratio: 1.0 / 3.0
                },
            ]
        );
    }

    #[test]
    fn does_not_equalize_same_direction_split_across_orthogonal_boundary() {
        let root = split(
            SplitDirection::Down,
            split_with_ratio(SplitDirection::Right, pane("a"), pane("b"), 0.7),
            split_with_ratio(SplitDirection::Right, pane("c"), pane("d"), 0.8),
        );

        assert_eq!(
            equalization_plan(&root, "d"),
            vec![RatioUpdate {
                path: vec![true],
                ratio: 0.5
            }]
        );
    }

    #[test]
    fn root_pane_creation_has_no_split_direction() {
        let root = pane("a");
        assert_eq!(equalization_plan(&root, "a"), vec![]);
    }

    #[test]
    fn skips_stale_creation_event_after_pane_was_split_again() {
        let root = split(
            SplitDirection::Right,
            pane("a"),
            split(SplitDirection::Down, pane("b"), pane("c")),
        );

        assert_eq!(equalization_plan(&root, "b"), vec![]);
    }

    #[test]
    fn equalizes_after_middle_pane_closes() {
        let root = split(
            SplitDirection::Down,
            split_with_ratio(SplitDirection::Right, pane("a"), pane("c"), 1.0 / 3.0),
            pane("d"),
        );

        assert_eq!(
            full_equalization_plan(&root),
            vec![RatioUpdate {
                path: vec![false],
                ratio: 0.5
            }]
        );
    }

    #[test]
    fn herdr_wire_fixtures() {
        let mut response: serde_json::Value = serde_json::from_str(
            r#"{
                "layout": {
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "root": {
                        "type": "split",
                        "direction": "right",
                        "ratio": 0.5,
                        "first": {"type": "pane", "pane_id": "w1:p1"},
                        "second": {
                            "type": "split",
                            "direction": "right",
                            "ratio": 0.5,
                            "first": {"type": "pane", "pane_id": "w1:p2"},
                            "second": {"type": "pane", "pane_id": "w1:p3"}
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        let layout: LayoutDescription = serde_json::from_value(response["layout"].take()).unwrap();
        assert_eq!(layout.tab_id, "w1:t1");

        let updates = equalization_plan(&layout.root, "w1:p3");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].path, Vec::<bool>::new());
    }
}
