use std::collections::HashMap;
use std::fs::{self, File};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use herdr_client::{
    Client, Environment, LayoutExportParams, LayoutNode, LayoutSetSplitRatioParams,
    PluginInvocation, SplitDirection, socket_scope_dir,
};

const SOCKET_TIMEOUT: Duration = Duration::from_secs(1);
// ponytail: Herdr layout.apply caps layouts at 24 panes; this also bounds
// retries if another client continuously changes the same layout.
const MAX_RATIO_UPDATES: usize = 64;

#[derive(Debug, PartialEq)]
struct RatioUpdate {
    path: Vec<bool>,
    ratio: f64,
}

enum Trigger {
    Startup,
    Moved(String),
    Created(String),
    Removed(String),
}

fn main() {
    if let Err(error) = run() {
        eprintln!("equalize-splits: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let environment = Environment::load()?;
    let plugin = environment.require_plugin()?;
    let socket_path = environment
        .socket_path
        .as_deref()
        .context("HERDR_SOCKET_PATH is not set")?;
    let trigger = match environment.invocation() {
        Some(PluginInvocation::Startup) => Trigger::Startup,
        Some(PluginInvocation::Event {
            name: "pane.created",
            event,
        }) => match event.data["pane"]["pane_id"].as_str() {
            Some(pane_id) if !pane_id.is_empty() => Trigger::Created(pane_id.to_owned()),
            _ => return Ok(()),
        },
        Some(PluginInvocation::Event {
            name: "pane.closed" | "pane.exited",
            event,
        }) => match event.data["pane_id"].as_str() {
            Some(pane_id) if !pane_id.is_empty() => Trigger::Removed(pane_id.to_owned()),
            _ => return Ok(()),
        },
        Some(PluginInvocation::Event {
            name: "pane.moved",
            event,
        }) => match event.data["previous_pane_id"].as_str() {
            Some(pane_id) if !pane_id.is_empty() => Trigger::Moved(pane_id.to_owned()),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    let run_dir = socket_scope_dir(&plugin.run_dir().join("sessions"), socket_path);
    fs::create_dir_all(&run_dir).context("create run directory")?;
    let lock_path = run_dir.join("equalize.lock");
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .context("open lock")?;
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).context("chmod lock")?;
    lock.lock().context("acquire lock")?;
    let client = Client::new(socket_path).with_timeout(SOCKET_TIMEOUT);
    let snapshot = client.snapshot().context("snapshot session")?;
    let cache_dir = socket_scope_dir(&plugin.cache_dir().join("sessions"), socket_path);
    fs::create_dir_all(&cache_dir).context("create cache directory")?;
    let pane_tabs_path = cache_dir.join("pane-tabs.json");
    let mut pane_tabs = match trigger {
        Trigger::Startup => HashMap::new(),
        _ => load_pane_tabs(&pane_tabs_path)?,
    };
    let removed_tab_id = refresh_pane_tabs(
        &mut pane_tabs,
        snapshot
            .panes
            .iter()
            .map(|pane| (pane.pane_id.as_str(), pane.tab_id.as_str())),
        match &trigger {
            Trigger::Moved(pane_id) | Trigger::Removed(pane_id) => Some(pane_id.as_str()),
            _ => None,
        },
    );
    save_pane_tabs(&pane_tabs_path, &pane_tabs)?;

    let created_pane_id = match &trigger {
        Trigger::Startup | Trigger::Moved(_) => return Ok(()),
        Trigger::Created(pane_id) => Some(pane_id),
        Trigger::Removed(_) => None,
    };
    let params = match created_pane_id {
        Some(pane_id) => LayoutExportParams {
            pane_id: Some(pane_id.clone()),
            tab_id: None,
        },
        None => {
            let Some(tab_id) = removed_tab_id
                .filter(|tab_id| snapshot.tabs.iter().any(|tab| tab.tab_id == *tab_id))
            else {
                return Ok(()); // The tab may have closed with its last pane.
            };
            LayoutExportParams {
                tab_id: Some(tab_id),
                pane_id: None,
            }
        }
    };
    let mut layout = client.export_layout(&params).context("export layout")?;

    for _ in 0..MAX_RATIO_UPDATES {
        let plan = match created_pane_id {
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
            .context("set split ratio")?;
    }
    bail!("layout kept changing while equalizing")
}

fn refresh_pane_tabs<'a>(
    pane_tabs: &mut HashMap<String, String>,
    panes: impl IntoIterator<Item = (&'a str, &'a str)>,
    removed_pane_id: Option<&str>,
) -> Option<String> {
    let removed_tab_id = removed_pane_id.and_then(|pane_id| pane_tabs.remove(pane_id));
    pane_tabs.extend(
        panes
            .into_iter()
            .map(|(pane_id, tab_id)| (pane_id.to_owned(), tab_id.to_owned())),
    );
    removed_tab_id
}

fn load_pane_tabs(path: &Path) -> Result<HashMap<String, String>> {
    match fs::read(path) {
        Ok(contents) => Ok(serde_json::from_slice(&contents).unwrap_or_default()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(error) => Err(error).context("read pane-to-tab cache"),
    }
}

fn save_pane_tabs(path: &Path, pane_tabs: &HashMap<String, String>) -> Result<()> {
    let parent = path
        .parent()
        .context("pane-to-tab cache path has no parent")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("open pane-to-tab cache temporary file")?;
    serde_json::to_writer(&mut temporary, pane_tabs).context("write pane-to-tab cache")?;
    temporary.persist(path).context("save pane-to-tab cache")?;
    Ok(())
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

    #[test]
    fn cache_save_replaces_the_file_with_private_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pane-tabs.json");
        let old_path = directory.path().join("old-cache.json");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::hard_link(&path, &old_path).unwrap();
        let pane_tabs = HashMap::from([("pane-a".into(), "tab-a".into())]);

        save_pane_tabs(&path, &pane_tabs).unwrap();

        assert_eq!(load_pane_tabs(&path).unwrap(), pane_tabs);
        assert_eq!(fs::read(&old_path).unwrap(), b"old");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

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
    fn removed_panes_keep_other_pending_mappings() {
        let mut pane_tabs = HashMap::from([
            ("pane-a".into(), "tab-a".into()),
            ("pane-b".into(), "tab-b".into()),
        ]);

        assert_eq!(
            refresh_pane_tabs(&mut pane_tabs, [], Some("pane-a")),
            Some("tab-a".into())
        );
        assert_eq!(pane_tabs.get("pane-b").map(String::as_str), Some("tab-b"));
        refresh_pane_tabs(&mut pane_tabs, [("pane-b", "tab-c")], None);
        assert_eq!(
            refresh_pane_tabs(&mut pane_tabs, [], Some("pane-b")),
            Some("tab-c".into())
        );
    }

    #[test]
    fn moved_pane_forgets_its_previous_id() {
        let mut pane_tabs = HashMap::from([("old-id".into(), "tab-a".into())]);

        refresh_pane_tabs(&mut pane_tabs, [("new-id", "tab-b")], Some("old-id"));

        assert_eq!(
            pane_tabs,
            HashMap::from([("new-id".into(), "tab-b".into())])
        );
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
        assert!(
            serde_json::from_value::<LayoutDescription>(serde_json::json!({
                "workspace_id": "w1",
                "tab_id": "w1:t1",
                "root": {"type": "pane", "pane_id": "w1:p1"}
            }))
            .is_err()
        );
        let mut response: serde_json::Value = serde_json::from_str(
            r#"{
                "layout": {
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "zoomed": false,
                    "focused_pane_id": "w1:p3",
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
