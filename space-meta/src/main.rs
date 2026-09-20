// Space Meta: publishes composite, PR-aware workspace tokens for the spaces sidebar.
use std::collections::HashMap;
use std::fs::{self, File};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use herdr_client::{Client, Environment, SessionSnapshot, WorkspaceInfo, socket_scope_dir};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);
const SOURCE_ID: &str = "gjermundgaraba.herdr-space-meta";
const PR_CACHE_TTL_SECS: u64 = 60;
const PENDING_FILE_PREFIX: &str = "space-meta.pending";
const BRAILLE_BLANK: char = '\u{2800}';

fn main() {
    if let Err(error) = run() {
        eprintln!("space-meta: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let environment = Environment::load().map_err(|error| error.to_string())?;
    let plugin = environment
        .require_plugin()
        .map_err(|error| error.to_string())?;
    let socket_path = environment
        .socket_path
        .as_deref()
        .ok_or("HERDR_SOCKET_PATH is not set")?;

    let run_dir = socket_scope_dir(&plugin.run_dir().join("sessions"), socket_path);
    fs::create_dir_all(&run_dir).map_err(|error| format!("create run directory: {error}"))?;
    let lock_path = run_dir.join("space-meta.lock");
    let pending_path = run_dir.join(format!(
        "{PENDING_FILE_PREFIX}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|error| format!("open lock: {error}"))?;
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("chmod lock: {error}"))?;
    mark_pending(&pending_path)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Ok(()),
        Err(fs::TryLockError::Error(error)) => {
            return Err(format!("acquire lock: {error}"));
        }
    }

    let cache_dir = socket_scope_dir(&plugin.cache_dir().join("sessions"), socket_path);
    fs::create_dir_all(&cache_dir).map_err(|error| format!("create cache directory: {error}"))?;
    let cache_path = cache_dir.join("pr-cache.json");
    if cache_path
        .try_exists()
        .map_err(|error| format!("check PR cache: {error}"))?
    {
        fs::set_permissions(&cache_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("chmod PR cache: {error}"))?;
    }
    let mut errors = Vec::new();
    let mut first_pass = true;

    loop {
        let pending_count = claim_pending(&run_dir)?;
        if pending_count == 0 {
            lock.unlock()
                .map_err(|error| format!("release unclaimed lock: {error}"))?;
            break;
        }
        let force_refresh_all = !first_pass || pending_count > 1;
        if let Err(error) = refresh(&environment, socket_path, &cache_path, force_refresh_all) {
            errors.push(error);
        }
        first_pass = false;
        lock.unlock()
            .map_err(|error| format!("release lock: {error}"))?;

        let pending = match has_pending(&run_dir) {
            Ok(pending) => pending,
            Err(error) => {
                errors.push(error);
                break;
            }
        };
        if !pending {
            break;
        }
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => break,
            Err(fs::TryLockError::Error(error)) => {
                errors.push(format!("reacquire lock: {error}"));
                break;
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn mark_pending(path: &Path) -> Result<(), String> {
    let mut options = File::options();
    options.create_new(true).write(true).mode(0o600);
    options
        .open(path)
        .map_err(|error| format!("mark refresh pending: {error}"))?;
    Ok(())
}

fn claim_pending(run_dir: &Path) -> Result<usize, String> {
    let mut count = 0;
    for entry in
        fs::read_dir(run_dir).map_err(|error| format!("list pending refreshes: {error}"))?
    {
        let entry = entry.map_err(|error| format!("read pending refresh: {error}"))?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(PENDING_FILE_PREFIX)
        {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => count += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("clear pending refresh: {error}")),
        }
    }
    Ok(count)
}

fn has_pending(run_dir: &Path) -> Result<bool, String> {
    for entry in
        fs::read_dir(run_dir).map_err(|error| format!("list pending refreshes: {error}"))?
    {
        let entry = entry.map_err(|error| format!("read pending refresh: {error}"))?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(PENDING_FILE_PREFIX)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn refresh(
    environment: &Environment,
    socket_path: &Path,
    cache_path: &Path,
    force_refresh_all: bool,
) -> Result<(), String> {
    let client = Client::new(socket_path).with_timeout(SOCKET_TIMEOUT);
    let snapshot = client
        .snapshot()
        .map_err(|error| format!("snapshot session: {error}"))?;
    let mut pr_cache = PrCache::load(cache_path);
    let refresh_all = force_refresh_all
        || environment.action_id.is_some()
        || environment.event_name.as_deref() == Some("startup");
    let refresh_workspace_id = (!refresh_all)
        .then(|| std::env::var("HERDR_WORKSPACE_ID").ok())
        .flatten();
    let mut errors = Vec::new();

    for (position, entry) in display_order(&snapshot).into_iter().enumerate() {
        let workspace = entry.workspace;
        let num = (position + 1).to_string();
        let refresh_external =
            refresh_all || refresh_workspace_id.as_deref() == Some(workspace.workspace_id.as_str());
        let sidebar_tokens = workspace_sidebar_tokens(
            &mut pr_cache,
            &snapshot,
            workspace,
            &num,
            entry.grouped_child,
            refresh_external,
        );
        let numbered_workspace = Some(sidebar_tokens.numbered_workspace.clone());
        let needs_update =
            |key: &str, value: &Option<String>| match (workspace.tokens.get(key), value) {
                (Some(current), Some(wanted)) => current != wanted,
                (None, None) => false,
                _ => true,
            };
        if !needs_update("numbered_workspace", &numbered_workspace)
            && !needs_update("branch_line", &sidebar_tokens.branch_line)
        {
            continue;
        }
        if let Err(error) = client.call_value(
            "workspace.report_metadata",
            &json!({
                "workspace_id": workspace.workspace_id,
                "source": SOURCE_ID,
                "tokens": {
                    "numbered_workspace": sidebar_tokens.numbered_workspace,
                    "branch_line": sidebar_tokens.branch_line,
                },
            }),
        ) {
            errors.push(format!(
                "report metadata for {}: {error}",
                workspace.workspace_id
            ));
        }
    }

    let active_cwds = snapshot
        .workspaces
        .iter()
        .filter_map(|workspace| workspace_cwd(&snapshot, workspace))
        .collect::<Vec<_>>();
    pr_cache.retain_cwds(&active_cwds);
    if let Err(error) = pr_cache.save(cache_path) {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Stable sidebar order with worktree groups expanded. A grouped repo's
/// non-linked parent is emitted at the position of its first member, with the
/// linked-worktree children right after it. Herdr does not expose desktop group
/// collapse state through its plugin API.
fn display_order(snapshot: &SessionSnapshot) -> Vec<DisplayEntry<'_>> {
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

    let mut emitted_groups = std::collections::HashSet::<&str>::new();
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

struct DisplayEntry<'a> {
    workspace: &'a WorkspaceInfo,
    grouped_child: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct SidebarTokens {
    numbered_workspace: String,
    branch_line: Option<String>,
}

/// Composite number/name first row for every space, plus an aligned branch
/// line for top-level spaces. Grouped worktree children use their branch as
/// the first-row label. Herdr's public plugin API does not expose whether a
/// child has a custom name, so that case cannot exactly match the built-in row.
fn workspace_sidebar_tokens(
    pr_cache: &mut PrCache,
    snapshot: &SessionSnapshot,
    workspace: &WorkspaceInfo,
    num: &str,
    grouped_child: bool,
    refresh_external: bool,
) -> SidebarTokens {
    let cwd = workspace_cwd(snapshot, workspace);
    let (branch, branch_refreshed) = cwd
        .as_deref()
        .map_or((None, false), |cwd| pr_cache.branch(cwd, refresh_external));
    let pr = cwd
        .as_deref()
        .zip(branch.as_deref())
        .and_then(|(cwd, branch)| {
            pr_cache.pr_number(cwd, branch, refresh_external || branch_refreshed)
        });
    let label = if grouped_child {
        grouped_child_display_label(&workspace.label, branch.as_deref())
    } else {
        workspace.label.clone()
    };
    format_sidebar_tokens(num, &label, branch.as_deref(), pr.as_deref(), grouped_child)
}

fn grouped_child_display_label(workspace_label: &str, branch: Option<&str>) -> String {
    branch
        .map(|branch| branch.strip_prefix("worktree/").unwrap_or(branch))
        .unwrap_or(workspace_label)
        .to_owned()
}

fn format_sidebar_tokens(
    num: &str,
    workspace_label: &str,
    branch: Option<&str>,
    pr: Option<&str>,
    grouped_child: bool,
) -> SidebarTokens {
    if grouped_child {
        let pr_suffix = pr.map_or_else(String::new, |number| format!(" #{number}"));
        return SidebarTokens {
            numbered_workspace: format!("{num} {workspace_label}{pr_suffix}"),
            branch_line: None,
        };
    }

    // Herdr prefixes continuation rows by 3 cells. The first row begins with
    // 1 cell, then `state_icon ` and `$numbered_workspace` begins. Within the
    // composite token, the label follows `num `. Padding by digit-count + 1
    // therefore puts the branch under that label exactly.
    let branch_line = branch.map(|branch| {
        let padding = BRAILLE_BLANK.to_string().repeat(num.chars().count() + 1);
        let pr_suffix = pr.map_or_else(String::new, |number| format!(" #{number}"));
        format!("{padding}{branch}{pr_suffix}")
    });
    SidebarTokens {
        numbered_workspace: format!("{num} {workspace_label}"),
        branch_line,
    }
}

fn workspace_cwd(snapshot: &SessionSnapshot, workspace: &WorkspaceInfo) -> Option<PathBuf> {
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

fn git_branch(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!branch.is_empty()).then_some(branch)
}

/// Branch and current-branch `gh pr view` answers cached per checkout so
/// unrelated workspace events do not launch subprocesses.
struct PrCache {
    entries: HashMap<String, PrCacheEntry>,
    dirty: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PrCacheEntry {
    // Require both keys while allowing null values.
    #[serde(deserialize_with = "Option::deserialize")]
    branch: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    number: Option<String>,
    #[serde(rename = "at")]
    stamped_at: u64,
}

impl PrCache {
    fn load(path: &Path) -> Self {
        let mut cache = Self {
            entries: HashMap::new(),
            dirty: false,
        };
        if let Ok(contents) = fs::read(path)
            && let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&contents)
        {
            for (key, value) in map {
                if value.is_object()
                    && let Ok(entry) = serde_json::from_value(value)
                {
                    cache.entries.insert(key, entry);
                }
            }
        }
        cache
    }

    fn branch(&mut self, cwd: &Path, refresh: bool) -> (Option<String>, bool) {
        let key = cwd.display().to_string();
        if !refresh && let Some(entry) = self.entries.get(&key) {
            return (entry.branch.clone(), false);
        }

        let branch = git_branch(cwd);
        match self.entries.get_mut(&key) {
            Some(entry) if entry.branch != branch => {
                entry.branch.clone_from(&branch);
                entry.number = None;
                entry.stamped_at = 0;
                self.dirty = true;
            }
            Some(_) => {}
            None => {
                self.entries.insert(
                    key,
                    PrCacheEntry {
                        branch: branch.clone(),
                        number: None,
                        stamped_at: 0,
                    },
                );
                self.dirty = true;
            }
        }
        (branch, true)
    }

    fn retain_cwds(&mut self, cwds: &[PathBuf]) {
        let keep = cwds
            .iter()
            .map(|cwd| cwd.display().to_string())
            .collect::<std::collections::HashSet<_>>();
        let before = self.entries.len();
        self.entries.retain(|key, _| keep.contains(key));
        self.dirty |= self.entries.len() != before;
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let parent = path.parent().ok_or("PR cache path has no parent")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| format!("open PR cache temporary file: {error}"))?;
        serde_json::to_writer(&mut temporary, &self.entries)
            .map_err(|error| format!("write PR cache: {error}"))?;
        temporary
            .persist(path)
            .map_err(|error| format!("save PR cache: {error}"))?;
        Ok(())
    }

    fn pr_number(&mut self, cwd: &Path, branch: &str, refresh: bool) -> Option<String> {
        let key = cwd.display().to_string();
        let entry = self.entries.get_mut(&key)?;
        if entry.branch.as_deref() != Some(branch) {
            return None;
        }
        let now = now_secs();
        let fresh = now.saturating_sub(entry.stamped_at) < PR_CACHE_TTL_SECS;
        if fresh || !refresh {
            return entry.number.clone();
        }
        let number = gh_pr_number(cwd);
        entry.number.clone_from(&number);
        entry.stamped_at = now;
        self.dirty = true;
        number
    }
}

fn gh_pr_number(cwd: &Path) -> Option<String> {
    let output = Command::new("gh")
        .current_dir(cwd)
        .args(["pr", "view", "--json", "number", "--jq", ".number"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let number = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!number.is_empty()).then_some(number)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_client::{AgentStatus, WorkspaceWorktreeInfo};

    fn snapshot(workspaces: Vec<WorkspaceInfo>) -> SessionSnapshot {
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

    fn workspace(id: &str, group: Option<(&str, bool)>) -> WorkspaceInfo {
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

    fn display_ids(snapshot: &SessionSnapshot) -> Vec<&str> {
        display_order(snapshot)
            .into_iter()
            .map(|entry| entry.workspace.workspace_id.as_str())
            .collect()
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

        assert_eq!(display_ids(&snapshot), ["first", "second", "third"]);
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
    fn pending_refreshes_are_coalesced_without_removing_other_files() {
        let temporary = tempfile::tempdir().unwrap();
        let run_dir = temporary.path().join("pending");
        fs::create_dir(&run_dir).unwrap();
        fs::write(run_dir.join("space-meta.pending-one"), b"").unwrap();
        fs::write(run_dir.join("space-meta.pending-two"), b"").unwrap();
        fs::write(run_dir.join("space-meta.lock"), b"").unwrap();

        assert!(has_pending(&run_dir).unwrap());
        assert_eq!(claim_pending(&run_dir).unwrap(), 2);
        assert!(!has_pending(&run_dir).unwrap());
        assert!(run_dir.join("space-meta.lock").exists());

        temporary.close().unwrap();
    }

    #[test]
    fn workspace_cwd_uses_worktree_or_first_pane_cwd() {
        let mut snapshot = snapshot(vec![workspace("workspace", None)]);
        snapshot.tabs = serde_json::from_value(json!([
            {
                "tab_id": "first-tab",
                "workspace_id": "workspace",
                "number": 1,
                "label": "first",
                "focused": true,
                "pane_count": 2,
                "agent_status": "unknown"
            }
        ]))
        .unwrap();
        snapshot.panes = serde_json::from_value(json!([
            {
                "pane_id": "first-pane",
                "terminal_id": "first-terminal",
                "workspace_id": "workspace",
                "tab_id": "first-tab",
                "focused": false,
                "cwd": "/first-pane",
                "foreground_cwd": "/first-pane/nested/other-repo",
                "agent_status": "unknown",
                "revision": 0
            },
            {
                "pane_id": "second-pane",
                "terminal_id": "second-terminal",
                "workspace_id": "workspace",
                "tab_id": "first-tab",
                "focused": false,
                "cwd": "/second-pane",
                "agent_status": "unknown",
                "revision": 0
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
    fn git_branch_detects_an_unborn_symbolic_head() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("unborn");
        let init = Command::new("git")
            .args(["init", "--quiet"])
            .arg(&repository)
            .status()
            .unwrap();
        assert!(init.success());
        let symbolic_ref = Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["symbolic-ref", "HEAD", "refs/heads/nascent"])
            .status()
            .unwrap();
        assert!(symbolic_ref.success());

        assert_eq!(git_branch(&repository).as_deref(), Some("nascent"));

        temporary.close().unwrap();
    }

    #[test]
    fn top_level_tokens_align_branch_under_composite_name() {
        let one_digit = format_sidebar_tokens("7", "project", Some("main"), Some("123"), false);
        assert_eq!(one_digit.numbered_workspace, "7 project",);
        assert_eq!(
            one_digit.branch_line,
            Some(format!("{}main #123", BRAILLE_BLANK.to_string().repeat(2)))
        );

        let two_digits = format_sidebar_tokens("16", "project", Some("main"), None, false);
        assert_eq!(two_digits.numbered_workspace, "16 project");
        assert_eq!(
            two_digits.branch_line,
            Some(format!("{}main", BRAILLE_BLANK.to_string().repeat(3)))
        );
    }

    #[test]
    fn grouped_child_folds_branch_and_pr_into_first_row() {
        assert_eq!(
            format_sidebar_tokens(
                "4",
                "gjermund/feature",
                Some("gjermund/feature"),
                Some("88"),
                true
            ),
            SidebarTokens {
                numbered_workspace: "4 gjermund/feature #88".into(),
                branch_line: None,
            }
        );
        assert_eq!(
            grouped_child_display_label("fallback-label", Some("worktree/feature")),
            "feature"
        );
        assert_eq!(
            format_sidebar_tokens("4", "fallback-label", None, None, true),
            SidebarTokens {
                numbered_workspace: "4 fallback-label".into(),
                branch_line: None,
            }
        );
    }

    #[test]
    fn cache_ignores_valid_json_entries_with_invalid_shapes() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("malformed-cache.json");
        fs::write(
            &path,
            br#"{
                "/ok": {"branch": "main", "number": "12", "at": 1},
                "/none": {"branch": null, "number": null, "at": 0},
                "/missing-branch": {"number": "1", "at": 1},
                "/missing-number": {"branch": "main", "at": 1},
                "/missing-at": {"branch": "main", "number": null},
                "/array": ["main", "12", 1],
                "/null": null,
                "/string": "main",
                "/integer": 1,
                "/boolean": true,
                "/wrong-branch": {"branch": 1, "number": null, "at": 0},
                "/wrong-number": {"branch": null, "number": 12, "at": 0},
                "/null-at": {"branch": null, "number": null, "at": null},
                "/string-at": {"branch": null, "number": null, "at": "1"},
                "/negative-at": {"branch": null, "number": null, "at": -1},
                "/float-at": {"branch": null, "number": null, "at": 1.0},
                "/overflow-at": {"branch": null, "number": null, "at": 18446744073709551616},
                "/max-at": {"branch": null, "number": null, "at": 18446744073709551615}
            }"#,
        )
        .unwrap();

        let mut cache = PrCache::load(&path);
        fs::remove_file(path).unwrap();

        assert_eq!(cache.entries.len(), 3);
        assert!(!cache.dirty);
        assert_eq!(cache.entries["/max-at"].stamped_at, u64::MAX);
        assert_eq!(
            cache.entries["/ok"],
            PrCacheEntry {
                branch: Some("main".into()),
                number: Some("12".into()),
                stamped_at: 1,
            }
        );
        assert_eq!(
            cache.entries["/none"],
            PrCacheEntry {
                branch: None,
                number: None,
                stamped_at: 0,
            }
        );
        assert_eq!(cache.branch(Path::new("/none"), false), (None, false));
        temporary.close().unwrap();
    }

    #[test]
    fn cache_save_replaces_the_file_with_private_permissions() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("private-cache.json");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let mut cache = PrCache {
            entries: HashMap::from([(
                "/repo".into(),
                PrCacheEntry {
                    branch: Some("main".into()),
                    number: Some("12".into()),
                    stamped_at: 1,
                },
            )]),
            dirty: false,
        };

        cache.save(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"old");
        cache.dirty = true;
        cache.save(&path).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
            json!({
                "/repo": {"branch": "main", "number": "12", "at": 1}
            })
        );
        temporary.close().unwrap();
    }
}
