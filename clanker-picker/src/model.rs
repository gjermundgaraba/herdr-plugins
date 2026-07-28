//! The agent inbox model: one flat list of every agent in the session,
//! attention-ranked — blocked first, then done, working, idle; most recent
//! state change first within each group.
//!
//! Filtering, search, and selection mechanics are copied from herdr
//! src/app/state.rs and src/app/actions.rs (Apache-2.0,
//! github.com/ogulcancelik/herdr); the workspace/tab tree the native
//! navigator builds around them is deliberately gone.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet};

use herdr_client::{AgentStatus, SessionSnapshot};

/// Herdr's public status collapses `(AgentState, seen)` into one value;
/// the picker ranking uses the two original dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

fn split(status: &AgentStatus) -> (AgentState, bool) {
    match status.as_str() {
        AgentStatus::BLOCKED => (AgentState::Blocked, true),
        AgentStatus::WORKING => (AgentState::Working, true),
        AgentStatus::DONE => (AgentState::Idle, false),
        AgentStatus::IDLE => (AgentState::Idle, true),
        _ => (AgentState::Unknown, true),
    }
}

/// One agent in the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub pane_id: String,
    /// Leads the row: usually the project name, the strongest identifier.
    pub workspace: String,
    /// Pane title/task; falls back through label, agent name, agent kind.
    pub label: String,
    /// Right-hand column: "agent · state", or just the state when the label
    /// already is the agent (the common no-title case).
    pub meta: String,
    /// The agent's working directory, home-shortened to `~/...`. Empty when
    /// unknown.
    pub path: String,
    pub status: AgentState,
    pub seen: bool,
    pub is_current: bool,
    /// Marked for focus (the `f` key): ranked above everything else.
    pub pinned: bool,
    pub search_text: String,
    /// Bottom detail line: full, untruncated context for the selected row.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StateFilter {
    Blocked,
    Working,
    Idle,
    Done,
}

pub fn text_matches_query(query: &str, text: &str) -> bool {
    let haystack = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|needle| haystack.contains(needle))
}

/// State filters and the text query are mutually exclusive, as in the native
/// navigator: activating one clears the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryKind {
    Empty,
    Text,
    State,
}

fn query_kind_for(query: &str, has_state_filters: bool) -> QueryKind {
    if has_state_filters {
        QueryKind::State
    } else if query.is_empty() {
        QueryKind::Empty
    } else {
        QueryKind::Text
    }
}

/// Verbatim from herdr; multi-select callers loop over the active set.
fn state_filter_matches(filter: StateFilter, state: AgentState, seen: bool) -> bool {
    match filter {
        StateFilter::Blocked => state == AgentState::Blocked,
        StateFilter::Working => state == AgentState::Working,
        StateFilter::Idle => state == AgentState::Idle && seen,
        StateFilter::Done => state == AgentState::Idle && !seen,
    }
}

pub fn state_label_text(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "unknown",
    }
}

/// The inbox ranking: who needs you first. Blocked is waiting on input,
/// done is finished and needs review, working and idle need nothing.
fn triage_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3, // done
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

// Public pane ids look like "w1:p3"; the trailing part is a base-32 public
// number. Copied from herdr src/workspace.rs.
const PUBLIC_ID_ALPHABET: &[u8; 32] = b"123456789ABCDEFGHJKMNPQRSTVWXYZ0";

fn decode_public_number(value: &str) -> Option<usize> {
    let mut decoded = 0usize;
    for ch in value.chars() {
        let digit = PUBLIC_ID_ALPHABET
            .iter()
            .position(|candidate| *candidate as char == ch)?;
        decoded = decoded
            .checked_mul(PUBLIC_ID_ALPHABET.len())?
            .checked_add(digit + 1)?;
    }
    Some(decoded)
}

fn public_pane_number(pane_id: &str) -> usize {
    pane_id
        .split_once(":p")
        .and_then(|(_, suffix)| decode_public_number(suffix))
        .unwrap_or(0)
}

fn shorten_home(path: &str, home: Option<&str>) -> String {
    match home {
        Some(home) if !home.is_empty() && path == home => "~".to_string(),
        Some(home)
            if !home.is_empty()
                && path.starts_with(home)
                && path[home.len()..].starts_with('/') =>
        {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_string(),
    }
}

/// This is an agent picker: panes whose row would say "shell" are dropped,
/// along with the tabs and workspaces left empty (they only matter for
/// labels and the detail line).
fn strip_agentless(snapshot: &mut SessionSnapshot) {
    let agent_panes: HashSet<String> = snapshot
        .agents
        .iter()
        .map(|agent| agent.pane_id.clone())
        .collect();
    snapshot.panes.retain(|pane| {
        pane.agent.is_some() || pane.display_agent.is_some() || agent_panes.contains(&pane.pane_id)
    });
    let live_tabs: HashSet<&str> = snapshot
        .panes
        .iter()
        .map(|pane| pane.tab_id.as_str())
        .collect();
    snapshot
        .tabs
        .retain(|tab| live_tabs.contains(tab.tab_id.as_str()));
    let live_workspaces: HashSet<&str> = snapshot
        .panes
        .iter()
        .map(|pane| pane.workspace_id.as_str())
        .collect();
    snapshot
        .workspaces
        .retain(|ws| live_workspaces.contains(ws.workspace_id.as_str()));
}

pub struct Picker {
    pub snapshot: SessionSnapshot,
    pub query: String,
    pub selected: usize,
    pub scroll: usize,
    pub search_focused: bool,
    pub state_filters: BTreeSet<StateFilter>,
    /// Pane ids marked for focus. Persisted by main.rs in the plugin state
    /// dir; the model only ranks by it.
    pub pinned: HashSet<String>,
    /// How many (two-line) rows fit in the body viewport, written by the UI
    /// on each draw. All selection/scroll math is in row units.
    pub visible_rows: usize,
    /// The pane that was focused when the popup opened. The popup itself is
    /// not a pane, so this comes from HERDR_PLUGIN_CONTEXT_JSON with the
    /// snapshot's focused_pane_id as fallback.
    pub current_pane_id: Option<String>,
}

impl Picker {
    /// Opens with the cursor on the FIRST row — the top of the inbox is the
    /// thing that needs you most (the native navigator opens on the current
    /// pane instead; the ◆ marker still shows where you came from).
    pub fn open(
        mut snapshot: SessionSnapshot,
        current_pane_id: Option<String>,
        mut pinned: HashSet<String>,
    ) -> Self {
        strip_agentless(&mut snapshot);
        // Drop focus marks whose panes no longer exist.
        pinned.retain(|pane_id| snapshot.panes.iter().any(|pane| pane.pane_id == *pane_id));
        Picker {
            current_pane_id: current_pane_id.or_else(|| snapshot.focused_pane_id.clone()),
            snapshot,
            query: String::new(),
            selected: 0,
            scroll: 0,
            search_focused: false,
            state_filters: BTreeSet::new(),
            pinned,
            visible_rows: 0,
        }
    }

    /// The 1 Hz refresh: swap the snapshot, keep the cursor sane.
    pub fn replace_snapshot(&mut self, mut snapshot: SessionSnapshot) {
        strip_agentless(&mut snapshot);
        self.snapshot = snapshot;
        self.clamp_selection();
    }

    fn filters_match(&self, state: AgentState, seen: bool) -> bool {
        self.state_filters
            .iter()
            .any(|filter| state_filter_matches(*filter, state, seen))
    }

    fn query_kind(&self) -> (String, QueryKind) {
        let query = self.query.trim().to_lowercase();
        let kind = query_kind_for(&query, !self.state_filters.is_empty());
        (query, kind)
    }

    /// The ranked inbox: build one row per pane, drop non-matches (a flat
    /// list has no hierarchy to keep as dimmed context), rank by
    /// (attention, recency).
    pub fn rows(&self) -> Vec<AgentRow> {
        let (query, kind) = self.query_kind();
        let agents_by_pane: HashMap<&str, &herdr_client::AgentInfo> = self
            .snapshot
            .agents
            .iter()
            .map(|agent| (agent.pane_id.as_str(), agent))
            .collect();
        let ws_labels: HashMap<&str, &str> = self
            .snapshot
            .workspaces
            .iter()
            .map(|ws| (ws.workspace_id.as_str(), ws.label.as_str()))
            .collect();
        let tab_labels: HashMap<&str, &str> = self
            .snapshot
            .tabs
            .iter()
            .map(|tab| (tab.tab_id.as_str(), tab.label.as_str()))
            .collect();
        let mut tabs_per_ws: HashMap<&str, usize> = HashMap::new();
        for tab in &self.snapshot.tabs {
            *tabs_per_ws.entry(tab.workspace_id.as_str()).or_default() += 1;
        }
        let home = std::env::var("HOME").ok();

        let mut rows = Vec::new();
        for pane in &self.snapshot.panes {
            let ws_label = ws_labels
                .get(pane.workspace_id.as_str())
                .copied()
                .unwrap_or("");
            let agent = agents_by_pane.get(pane.pane_id.as_str()).copied();
            let agent_name = agent.and_then(|agent| agent.name.as_deref());
            let stripped_title = agent.and_then(|agent| agent.terminal_title_stripped.as_deref());
            let pane_number = public_pane_number(&pane.pane_id);
            // Native chain: effective_title -> manual_label -> agent_name ->
            // effective_agent_label -> launch_label -> "pane N", with the
            // agent's stripped terminal title (the live task summary) slotted
            // in as the effective title. No launch_argv on the wire.
            let label = pane
                .title
                .clone()
                .or_else(|| pane.label.clone())
                .or_else(|| stripped_title.map(str::to_string))
                .or_else(|| agent_name.map(str::to_string))
                .or_else(|| pane.agent.clone())
                .unwrap_or_else(|| format!("pane {pane_number}"));
            let agent_label = pane
                .display_agent
                .as_deref()
                .or(agent_name)
                .or(pane.agent.as_deref());
            let (state, seen) = split(&pane.agent_status);
            let status_label = pane
                .state_labels
                .get(state_label_text(state, seen))
                .cloned()
                .unwrap_or_else(|| state_label_text(state, seen).to_string());

            let meta = match agent_label {
                Some(agent) if agent != label => format!("{agent} · {status_label}"),
                _ => status_label.clone(),
            };
            let path = agent
                .and_then(|agent| agent.foreground_cwd.as_deref().or(agent.cwd.as_deref()))
                .map(|path| shorten_home(path, home.as_deref()))
                .unwrap_or_default();
            let search_text = format!("{ws_label} {label} {meta} {path}").to_lowercase();

            let mut detail_parts = vec![ws_label.to_string()];
            let multi_tab = tabs_per_ws
                .get(pane.workspace_id.as_str())
                .is_some_and(|count| *count > 1);
            if multi_tab {
                let tab_label = tab_labels.get(pane.tab_id.as_str()).copied().unwrap_or("");
                detail_parts.push(format!("tab: {tab_label}"));
            }
            detail_parts.push(format!("pane {pane_number}"));
            // The effective label, unless it is just the agent (or pane
            // number) fallback already covered by the parts around it.
            if agent_label != Some(label.as_str()) && label != format!("pane {pane_number}") {
                detail_parts.push(label.clone());
            }
            if let Some(agent) = agent_label {
                detail_parts.push(agent.to_string());
            }
            detail_parts.push(status_label);
            if !path.is_empty() {
                detail_parts.push(path.clone());
            }

            rows.push(AgentRow {
                pane_id: pane.pane_id.clone(),
                workspace: ws_label.to_string(),
                label,
                path,
                meta,
                status: state,
                seen,
                is_current: self.current_pane_id.as_deref() == Some(pane.pane_id.as_str()),
                pinned: self.pinned.contains(pane.pane_id.as_str()),
                search_text,
                detail: detail_parts.join(" · "),
            });
        }

        let mut rows: Vec<_> = match kind {
            QueryKind::Empty => rows,
            QueryKind::State => rows
                .into_iter()
                .filter(|row| self.filters_match(row.status, row.seen))
                .collect(),
            QueryKind::Text => rows
                .into_iter()
                .filter(|row| text_matches_query(&query, &row.search_text))
                .collect(),
        };

        // Focus marks beat everything; then attention, then recency. Stable
        // sort: agents that never changed state (seq 0) keep snapshot order
        // among themselves.
        rows.sort_by_key(|row| {
            let seq = agents_by_pane
                .get(row.pane_id.as_str())
                .map(|agent| agent.state_change_seq)
                .unwrap_or(0);
            (
                Reverse(row.pinned),
                Reverse(triage_priority(row.status, row.seen)),
                Reverse(seq),
            )
        });
        rows
    }

    /// Toggle the focus mark on the selected agent, keeping the cursor on
    /// that agent as it moves to (or from) the top of the list.
    pub fn toggle_pin(&mut self) {
        let Some(row) = self.rows().get(self.selected).cloned() else {
            return;
        };
        if !self.pinned.remove(&row.pane_id) {
            self.pinned.insert(row.pane_id.clone());
        }
        if let Some(idx) = self
            .rows()
            .iter()
            .position(|other| other.pane_id == row.pane_id)
        {
            self.selected = idx;
        }
        self.ensure_selection_visible();
    }

    /// Header summary in inbox order: "N blocked · N done · N working",
    /// zero groups omitted; plain agent count when nothing is active.
    pub fn activity_summary(&self) -> String {
        let mut blocked = 0usize;
        let mut done = 0usize;
        let mut working = 0usize;
        for pane in &self.snapshot.panes {
            match split(&pane.agent_status) {
                (AgentState::Blocked, _) => blocked += 1,
                (AgentState::Idle, false) => done += 1,
                (AgentState::Working, _) => working += 1,
                _ => {}
            }
        }
        let mut parts = Vec::new();
        if blocked > 0 {
            parts.push(format!("{blocked} blocked"));
        }
        if done > 0 {
            parts.push(format!("{done} done"));
        }
        if working > 0 {
            parts.push(format!("{working} working"));
        }
        if parts.is_empty() {
            return format!("{} agents", self.snapshot.panes.len());
        }
        parts.join(" · ")
    }

    pub fn ensure_selection_visible(&mut self) {
        let viewport = self.visible_rows;
        if viewport == 0 {
            self.scroll = 0;
            return;
        }
        let max_scroll = self.rows().len().saturating_sub(viewport);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll.saturating_add(viewport) {
            self.scroll = self.selected.saturating_add(1).saturating_sub(viewport);
        }
        self.scroll = self.scroll.min(max_scroll);
    }

    pub fn max_scroll(&self, viewport: usize) -> usize {
        if viewport == 0 {
            return 0;
        }
        self.rows().len().saturating_sub(viewport)
    }

    /// After a mouse-wheel scroll, snap the selection to the top of the
    /// viewport.
    pub fn align_selection_to_scroll(&mut self) {
        self.selected = self.scroll;
        self.clamp_selection();
    }

    /// Single steps wrap around at the ends; larger jumps (half-page) clamp.
    pub fn move_selection(&mut self, delta: isize) {
        let count = self.rows().len();
        if count == 0 {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        let current = self.selected.min(count - 1) as isize;
        self.selected = match delta {
            1 if current == count as isize - 1 => 0,
            -1 if current == 0 => count - 1,
            _ => (current + delta).clamp(0, count as isize - 1) as usize,
        };
        self.ensure_selection_visible();
    }

    /// After the query or state filters change, the best match is the top of
    /// the ranked, filtered list.
    pub fn select_first_match(&mut self) {
        let (_, kind) = self.query_kind();
        if !matches!(kind, QueryKind::Empty) {
            self.selected = 0;
            self.scroll = 0;
        }
        self.clamp_selection();
    }

    pub fn clamp_selection(&mut self) {
        let count = self.rows().len();
        self.selected = self.selected.min(count.saturating_sub(1));
        self.ensure_selection_visible();
    }

    /// Toggle a state filter in the multi-select set (deviation from native
    /// replace-on-keypress), clearing the text query like native does.
    pub fn toggle_state_filter(&mut self, filter: StateFilter) {
        self.query.clear();
        if !self.state_filters.remove(&filter) {
            self.state_filters.insert(filter);
        }
        self.select_first_match();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = r#"{
        "version": "0.7.5",
        "protocol": 17,
        "focused_workspace_id": "w1",
        "focused_tab_id": "w1:t1",
        "focused_pane_id": "w1:p1",
        "workspaces": [
            {"workspace_id": "w1", "number": 1, "label": "api", "focused": true,
             "pane_count": 2, "tab_count": 1, "active_tab_id": "w1:t1",
             "agent_status": "done"},
            {"workspace_id": "w2", "number": 2, "label": "web", "focused": false,
             "pane_count": 3, "tab_count": 2, "active_tab_id": "w2:t1",
             "agent_status": "blocked"}
        ],
        "tabs": [
            {"tab_id": "w1:t1", "workspace_id": "w1", "number": 1, "label": "main",
             "focused": true, "pane_count": 2, "agent_status": "done"},
            {"tab_id": "w2:t1", "workspace_id": "w2", "number": 1, "label": "1",
             "focused": false, "pane_count": 1, "agent_status": "blocked"},
            {"tab_id": "w2:t2", "workspace_id": "w2", "number": 2, "label": "tests",
             "focused": false, "pane_count": 2, "agent_status": "working"}
        ],
        "panes": [
            {"pane_id": "w1:p1", "terminal_id": "term-1", "workspace_id": "w1",
             "tab_id": "w1:t1", "focused": true, "revision": 1,
             "agent": "claude", "agent_status": "done",
             "state_labels": {"done": "Done"}},
            {"pane_id": "w1:p2", "terminal_id": "term-2", "workspace_id": "w1",
             "tab_id": "w1:t1", "focused": false, "revision": 1,
             "agent_status": "unknown"},
            {"pane_id": "w2:p1", "terminal_id": "term-3", "workspace_id": "w2",
             "tab_id": "w2:t1", "focused": false, "revision": 1,
             "agent": "claude", "title": "fix tests", "agent_status": "blocked"},
            {"pane_id": "w2:p2", "terminal_id": "term-4", "workspace_id": "w2",
             "tab_id": "w2:t2", "focused": false, "revision": 1,
             "agent": "codex", "agent_status": "working"},
            {"pane_id": "w2:p3", "terminal_id": "term-5", "workspace_id": "w2",
             "tab_id": "w2:t2", "focused": false, "revision": 1,
             "agent": "claude", "agent_status": "done"}
        ],
        "agents": [
            {"terminal_id": "term-1", "pane_id": "w1:p1", "workspace_id": "w1",
             "tab_id": "w1:t1", "focused": true, "revision": 1,
             "agent_status": "done", "name": "fixer", "state_change_seq": 3,
             "cwd": "/tmp/api"},
            {"terminal_id": "term-3", "pane_id": "w2:p1", "workspace_id": "w2",
             "tab_id": "w2:t1", "focused": false, "revision": 1,
             "agent_status": "blocked", "state_change_seq": 7,
             "cwd": "/x/one", "foreground_cwd": "/x/two"},
            {"terminal_id": "term-4", "pane_id": "w2:p2", "workspace_id": "w2",
             "tab_id": "w2:t2", "focused": false, "revision": 1,
             "agent_status": "working", "state_change_seq": 1},
            {"terminal_id": "term-5", "pane_id": "w2:p3", "workspace_id": "w2",
             "tab_id": "w2:t2", "focused": false, "revision": 1,
             "agent_status": "done", "state_change_seq": 5,
             "terminal_title_stripped": "review docs"}
        ],
        "layouts": []
    }"#;

    fn picker() -> Picker {
        let snapshot: SessionSnapshot = serde_json::from_str(SNAPSHOT).unwrap();
        Picker::open(snapshot, None, HashSet::new())
    }

    #[test]
    fn ranks_by_attention_then_recency_and_strips_shells() {
        let rows = picker().rows();
        // blocked (seq 7) > done (seq 5) > done (seq 3) > working (seq 1);
        // the shell pane w1:p2 is gone entirely.
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, vec!["fix tests", "review docs", "fixer", "codex"]);
        let ids: Vec<&str> = rows.iter().map(|row| row.pane_id.as_str()).collect();
        assert_eq!(ids, vec!["w2:p1", "w2:p3", "w1:p1", "w2:p2"]);
    }

    #[test]
    fn label_chain_and_meta_shape() {
        let rows = picker().rows();
        // title beats everything; meta = agent · state.
        assert_eq!(rows[0].workspace, "web");
        assert_eq!(rows[0].label, "fix tests");
        assert_eq!(rows[0].meta, "claude · blocked");
        // agent name beats agent kind; the agent drops out of the meta when
        // the label already is the agent; state_labels override the plain
        // word.
        assert_eq!(rows[2].workspace, "api");
        assert_eq!(rows[2].label, "fixer");
        assert_eq!(rows[2].meta, "Done");
        // The stripped terminal title (live task summary) becomes the label
        // when the pane has no title of its own.
        assert_eq!(rows[1].label, "review docs");
        assert_eq!(rows[1].meta, "claude · done");
    }

    #[test]
    fn opens_at_top_with_current_marker() {
        let picker = picker();
        // The top of the inbox is what needs you most; the ◆ marker still
        // points at where you came from (w1:p1 ranks third: done, seq 3).
        assert_eq!(picker.selected, 0);
        assert!(picker.rows()[2].is_current);
    }

    #[test]
    fn state_filters_narrow_the_ranked_list() {
        let mut picker = picker();
        picker.toggle_state_filter(StateFilter::Done);
        let labels: Vec<String> = picker.rows().iter().map(|row| row.label.clone()).collect();
        assert_eq!(labels, vec!["review docs", "fixer"]);
        assert_eq!(picker.selected, 0);

        picker.toggle_state_filter(StateFilter::Blocked);
        let labels: Vec<String> = picker.rows().iter().map(|row| row.label.clone()).collect();
        assert_eq!(labels, vec!["fix tests", "review docs", "fixer"]);

        // Toggling done off again leaves only blocked.
        picker.toggle_state_filter(StateFilter::Done);
        let labels: Vec<String> = picker.rows().iter().map(|row| row.label.clone()).collect();
        assert_eq!(labels, vec!["fix tests"]);
    }

    #[test]
    fn search_matches_workspace_agent_and_title() {
        let mut picker = picker();
        picker.query = "web claude".to_string();
        let labels: Vec<String> = picker.rows().iter().map(|row| row.label.clone()).collect();
        assert_eq!(labels, vec!["fix tests", "review docs"]);

        picker.query = "api".to_string();
        let labels: Vec<String> = picker.rows().iter().map(|row| row.label.clone()).collect();
        assert_eq!(labels, vec!["fixer"]);
    }

    #[test]
    fn detail_lines_carry_full_context() {
        let rows = picker().rows();
        assert_eq!(
            rows[0].detail,
            "web · tab: 1 · pane 1 · fix tests · claude · blocked · /x/two"
        );
        // Single-tab workspace omits the tab part.
        assert_eq!(rows[2].detail, "api · pane 1 · fixer · Done · /tmp/api");
        assert_eq!(
            rows[1].detail,
            "web · tab: tests · pane 3 · review docs · claude · done"
        );
    }

    #[test]
    fn paths_prefer_foreground_cwd_and_shorten_home() {
        let rows = picker().rows();
        assert_eq!(rows[0].path, "/x/two");
        assert_eq!(rows[2].path, "/tmp/api");
        assert_eq!(rows[3].path, ""); // no cwd on the wire

        assert_eq!(shorten_home("/home/gg/ws/x", Some("/home/gg")), "~/ws/x");
        assert_eq!(shorten_home("/home/gg", Some("/home/gg")), "~");
        assert_eq!(
            shorten_home("/home/ggx/ws", Some("/home/gg")),
            "/home/ggx/ws"
        );
        assert_eq!(shorten_home("/etc", None), "/etc");
    }

    #[test]
    fn focus_marks_rank_above_everything_and_cursor_follows() {
        let mut picker = picker();
        // codex: working, lowest seq -> bottom of the list.
        picker.selected = 3;
        assert_eq!(picker.rows()[3].label, "codex");
        picker.toggle_pin();
        let rows = picker.rows();
        assert_eq!(rows[0].label, "codex");
        assert!(rows[0].pinned);
        assert_eq!(picker.selected, 0);
        // Unpin from the top; codex sinks back and the cursor follows.
        picker.toggle_pin();
        assert_eq!(picker.rows()[3].label, "codex");
        assert_eq!(picker.selected, 3);
    }

    #[test]
    fn stale_focus_marks_are_pruned_on_open() {
        let snapshot: SessionSnapshot = serde_json::from_str(SNAPSHOT).unwrap();
        let picker = Picker::open(
            snapshot,
            None,
            HashSet::from(["w1:p1".to_string(), "gone:p9".to_string()]),
        );
        assert_eq!(picker.pinned, HashSet::from(["w1:p1".to_string()]));
        assert!(picker.rows()[0].pinned);
        assert_eq!(picker.rows()[0].label, "fixer");
    }

    #[test]
    fn single_step_moves_wrap_at_the_ends() {
        let mut picker = picker();
        picker.selected = 0;
        picker.move_selection(-1);
        assert_eq!(picker.selected, 3);
        picker.move_selection(1);
        assert_eq!(picker.selected, 0);
        // Half-page jumps clamp instead of wrapping.
        picker.move_selection(2);
        picker.move_selection(5);
        assert_eq!(picker.selected, 3);
    }

    #[test]
    fn activity_summary_in_inbox_order() {
        assert_eq!(
            picker().activity_summary(),
            "1 blocked · 2 done · 1 working"
        );
    }

    #[test]
    fn text_matches_query_is_and_of_substrings() {
        assert!(text_matches_query("fix api", "API fixer running"));
        assert!(!text_matches_query("fix web", "API fixer running"));
        assert!(text_matches_query("", "anything"));
    }

    #[test]
    fn split_status_roundtrip() {
        assert_eq!(
            split(&AgentStatus::from(AgentStatus::DONE)),
            (AgentState::Idle, false)
        );
    }

    #[test]
    fn decode_public_numbers() {
        assert_eq!(public_pane_number("w1:p1"), 1);
        assert_eq!(public_pane_number("w1:p3"), 3);
        assert_eq!(public_pane_number("nonsense"), 0);
    }
}
