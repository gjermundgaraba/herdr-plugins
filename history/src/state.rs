//! Focus history state and replay reconciliation.

const MAX_ENTRIES: usize = 100;
const ECHO_TTL_MS: u64 = 1_500;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Echo {
    pub(crate) pane_id: String,
    pub(crate) created_at_ms: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct State {
    pub(crate) entries: Vec<String>,
    pub(crate) cursor: usize,
    pub(crate) echoes: Vec<Echo>,
}

impl State {
    pub(crate) fn fresh() -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
            echoes: Vec::new(),
        }
    }

    pub(crate) fn expire_echoes(&mut self, now: u64) {
        self.echoes
            .retain(|echo| now.saturating_sub(echo.created_at_ms) < ECHO_TTL_MS);
    }

    pub(crate) fn record(&mut self, pane_id: String) {
        if let Some(index) = self.echoes.iter().position(|echo| echo.pane_id == pane_id) {
            self.echoes.drain(..=index);
            return;
        }
        if self.entries.get(self.cursor) == Some(&pane_id) {
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(pane_id);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.drain(..self.entries.len() - MAX_ENTRIES);
        }
        self.cursor = self.entries.len() - 1;
    }

    pub(crate) fn plan_jump(&self, step: isize) -> Option<(usize, String)> {
        let index = self.cursor.checked_add_signed(step)?;
        self.entries
            .get(index)
            .cloned()
            .map(|pane_id| (index, pane_id))
    }

    pub(crate) fn push_echo(&mut self, pane_id: String, now: u64) {
        self.echoes.push(Echo {
            pane_id,
            created_at_ms: now,
        });
    }

    pub(crate) fn focus_failed(&mut self, index: usize) {
        let target = self.entries.remove(index);
        if index <= self.cursor {
            self.cursor -= 1;
        }
        if let Some(index) = self.echoes.iter().rposition(|echo| echo.pane_id == target) {
            self.echoes.remove(index);
        }
    }

    pub(crate) fn cancel_echo(&mut self, pane_id: &str) {
        if let Some(index) = self.echoes.iter().rposition(|echo| echo.pane_id == pane_id) {
            self.echoes.remove(index);
        }
    }
}

pub(crate) fn record_snapshot_events(
    state: &mut State,
    snapshot_pane: Option<String>,
    events: Vec<String>,
) {
    // Events collected while the snapshot was in flight are ordered; the
    // snapshot is only a fallback when no event crossed that boundary.
    if events.is_empty() {
        if let Some(pane_id) = snapshot_pane {
            state.record(pane_id);
        }
    } else {
        for pane_id in events {
            state.record(pane_id);
        }
    }
}

pub(crate) fn replay_suffix(events: &[String], baseline: Option<&str>) -> usize {
    baseline
        .and_then(|pane| events.iter().rposition(|event| event == pane))
        .map_or(events.len(), |index| index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_visits_and_truncates_forward_history() {
        let mut state = visited(&["A", "B", "B", "C"]);
        assert_eq!(state.entries, ["A", "B", "C"]);
        state.cursor = 0;
        state.record("D".into());
        assert_eq!(state.entries, ["A", "D"]);
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn ordered_echoes_preserve_rapid_double_back() {
        let mut state = visited(&["A", "B", "C"]);
        state.push_echo("B".into(), 0);
        state.cursor = 1;
        state.push_echo("A".into(), 0);
        state.cursor = 0;
        state.record("B".into());
        state.record("A".into());
        assert_eq!(state.entries, ["A", "B", "C"]);
        assert_eq!(state.cursor, 0);
        assert!(state.echoes.is_empty());
    }

    #[test]
    fn prunes_dead_panes_in_both_directions() {
        let mut back = visited(&["A", "B", "C", "D"]);
        back.push_echo("C".into(), 0);
        back.focus_failed(2);
        assert_eq!(back.entries, ["A", "B", "D"]);
        assert_eq!(back.plan_jump(-1), Some((1, "B".into())));

        let mut forward = visited(&["A", "B", "C"]);
        forward.cursor = 0;
        forward.focus_failed(1);
        assert_eq!(forward.entries, ["A", "C"]);
        assert_eq!(forward.plan_jump(1), Some((1, "C".into())));
    }

    #[test]
    fn stops_at_ends_and_caps_history() {
        let mut state = fresh();
        assert_eq!(state.plan_jump(-1), None);
        for index in 0..MAX_ENTRIES + 20 {
            state.record(format!("p{index}"));
        }
        assert_eq!(state.entries.len(), MAX_ENTRIES);
        assert_eq!(state.entries[0], "p20");
        assert_eq!(state.cursor, MAX_ENTRIES - 1);
        assert_eq!(state.plan_jump(1), None);
    }

    #[test]
    fn expires_and_cancels_echoes() {
        let mut state = fresh();
        state.push_echo("A".into(), 1_000);
        state.expire_echoes(1_000 + ECHO_TTL_MS);
        assert!(state.echoes.is_empty());

        state.push_echo("A".into(), 2_000);
        state.push_echo("B".into(), 2_000);
        state.cancel_echo("A");
        assert_eq!(state.echoes[0].pane_id, "B");
    }

    #[test]
    fn retained_replay_keeps_only_events_after_the_snapshot_boundary() {
        let events = ["B", "C", "B", "D"].map(str::to_owned);
        assert_eq!(replay_suffix(&events, Some("B")), 3);
        assert_eq!(replay_suffix(&events, Some("A")), events.len());
    }

    #[test]
    fn snapshot_reconciliation_preserves_queued_focus_order() {
        let mut state = visited(&["C"]);
        record_snapshot_events(
            &mut state,
            Some("E".into()),
            ["D", "E"].map(str::to_owned).into(),
        );
        assert_eq!(state.entries, ["C", "D", "E"]);

        let mut fallback = visited(&["C"]);
        record_snapshot_events(&mut fallback, Some("E".into()), Vec::new());
        assert_eq!(fallback.entries, ["C", "E"]);
    }

    fn visited(panes: &[&str]) -> State {
        let mut state = fresh();
        for pane in panes {
            state.record((*pane).into());
        }
        state
    }

    fn fresh() -> State {
        State::fresh()
    }
}
