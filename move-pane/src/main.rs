//! Move the focused pane to another tab. With one tab the pane goes straight
//! into a new tab; otherwise a popup lists the destinations.
use std::{process::ExitCode, time::Duration};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use herdr_client::{Client, Environment, TabInfo};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState},
};
use serde_json::json;

const ENV_PANE: &str = "HERDR_MOVE_PANE_ID";
const ENV_WORKSPACE: &str = "HERDR_MOVE_WORKSPACE_ID";
const ENV_TAB: &str = "HERDR_MOVE_TAB_ID";
const NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    let pick = std::env::args().nth(1).as_deref() == Some("pick");
    let result = if pick { run_pick() } else { run_action() };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("move-pane: {error:#}");
            if let Ok(client) = Client::from_env() {
                let _ = client.with_timeout(NOTIFICATION_TIMEOUT).call_value(
                    "notification.show",
                    &json!({"title": "Move pane failed", "body": format!("{error:#}")}),
                );
            }
            ExitCode::FAILURE
        }
    }
}

/// The action: move directly when the workspace has one tab, else open the
/// popup with the pane identity pinned in its environment.
fn run_action() -> Result<()> {
    let environment = Environment::load()?;
    let client = Client::from_env()?;
    let pane = client
        .current_pane(environment.pane_id.as_deref())
        .context("read focused pane")?;
    let destinations = destinations(&tabs_in(&client, &pane.workspace_id)?, &pane.tab_id);
    if let [only] = destinations.as_slice() {
        return move_pane(&client, &pane.pane_id, only);
    }
    let plugin_id = environment
        .plugin_id
        .context("HERDR_PLUGIN_ID is not set")?;
    client
        .call_value(
            "plugin.pane.open",
            &json!({
                "plugin_id": plugin_id,
                "entrypoint": "pick",
                "placement": "popup",
                "width": 56,
                "height": destinations.len() + 4,
                "focus": true,
                "env": {
                    ENV_PANE: pane.pane_id,
                    ENV_WORKSPACE: pane.workspace_id,
                    ENV_TAB: pane.tab_id,
                },
            }),
        )
        .context("open the move-pane popup")?;
    Ok(())
}

fn run_pick() -> Result<()> {
    let pane_id = required(ENV_PANE)?;
    let workspace_id = required(ENV_WORKSPACE)?;
    let tab_id = required(ENV_TAB)?;
    let client = Client::from_env()?;
    let destinations = destinations(&tabs_in(&client, &workspace_id)?, &tab_id);
    let mut terminal = ratatui::try_init()?;
    let choice = choose(&mut terminal, &destinations);
    ratatui::restore();
    match choice? {
        Some(destination) => move_pane(&client, &pane_id, destination),
        None => Ok(()),
    }
}

fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{name} is not set; run this through the move action"))
}

fn tabs_in(client: &Client, workspace_id: &str) -> Result<Vec<TabInfo>> {
    let mut tabs: Vec<TabInfo> = client
        .snapshot()
        .context("read session snapshot")?
        .tabs
        .into_iter()
        .filter(|tab| tab.workspace_id == workspace_id)
        .collect();
    tabs.sort_by_key(|tab| tab.number);
    Ok(tabs)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    NewTab { workspace_id: String },
    Tab(TabInfo),
}

impl Destination {
    fn label(&self) -> &str {
        match self {
            Self::NewTab { .. } => "New tab",
            Self::Tab(tab) => &tab.label,
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::NewTab { .. } => "move into a new tab".into(),
            Self::Tab(tab) if tab.pane_count == 1 => "1 pane".into(),
            Self::Tab(tab) => format!("{} panes", tab.pane_count),
        }
    }
}

/// A new tab first, then every other tab of the workspace in tab order.
fn destinations(tabs: &[TabInfo], current_tab_id: &str) -> Vec<Destination> {
    let workspace_id = tabs
        .first()
        .map(|tab| tab.workspace_id.clone())
        .unwrap_or_default();
    std::iter::once(Destination::NewTab { workspace_id })
        .chain(
            tabs.iter()
                .filter(|tab| tab.tab_id != current_tab_id)
                .cloned()
                .map(Destination::Tab),
        )
        .collect()
}

fn move_pane(client: &Client, pane_id: &str, destination: &Destination) -> Result<()> {
    let destination = match destination {
        Destination::NewTab { workspace_id } => {
            json!({"type": "new_tab", "workspace_id": workspace_id})
        }
        Destination::Tab(tab) => json!({"type": "tab", "tab_id": tab.tab_id, "split": "right"}),
    };
    client
        .call_value(
            "pane.move",
            &json!({"pane_id": pane_id, "destination": destination, "focus": true}),
        )
        .context("move pane")?;
    Ok(())
}

fn choose<'a>(
    terminal: &mut ratatui::DefaultTerminal,
    destinations: &'a [Destination],
) -> Result<Option<&'a Destination>> {
    let mut selected = 0;
    loop {
        terminal.draw(|frame| render(frame, destinations, selected))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match step(key.code, key.modifiers, selected, destinations.len()) {
            Step::Move(next) => selected = next,
            Step::Confirm(index) => return Ok(destinations.get(index)),
            Step::Cancel => return Ok(None),
            Step::Ignore => {}
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Move(usize),
    Confirm(usize),
    Cancel,
    Ignore,
}

fn step(code: KeyCode, modifiers: KeyModifiers, selected: usize, len: usize) -> Step {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let last = len.saturating_sub(1);
    match code {
        KeyCode::Esc | KeyCode::Char('q') => Step::Cancel,
        KeyCode::Char('c') if ctrl => Step::Cancel,
        KeyCode::Enter => Step::Confirm(selected),
        KeyCode::Down | KeyCode::Char('j') => Step::Move((selected + 1).min(last)),
        KeyCode::Char('n') if ctrl => Step::Move((selected + 1).min(last)),
        KeyCode::Up | KeyCode::Char('k') => Step::Move(selected.saturating_sub(1)),
        KeyCode::Char('p') if ctrl => Step::Move(selected.saturating_sub(1)),
        KeyCode::Char(digit @ '1'..='9') => {
            let index = digit as usize - '1' as usize;
            if index < len {
                Step::Confirm(index)
            } else {
                Step::Ignore
            }
        }
        _ => Step::Ignore,
    }
}

fn render(frame: &mut Frame<'_>, destinations: &[Destination], selected: usize) {
    let muted = Style::new().fg(Color::DarkGray);
    let [title, body, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(
        Line::styled("Move pane to", Style::new().add_modifier(Modifier::BOLD)),
        title,
    );
    let items = destinations.iter().enumerate().map(|(index, destination)| {
        ListItem::new(Line::from(vec![
            Span::styled(format!("{:>2} ", index + 1), muted),
            Span::raw(destination.label().to_owned()),
            Span::styled(format!("  {}", destination.detail()), muted),
        ]))
    });
    let list = List::new(items).highlight_style(Style::new().bg(Color::Blue).fg(Color::White));
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, body, &mut state);
    frame.render_widget(
        Line::from(vec![
            Span::styled("j/k", Style::new().fg(Color::Cyan)),
            Span::styled(" move  ", muted),
            Span::styled("1-9", Style::new().fg(Color::Cyan)),
            Span::styled(" jump  ", muted),
            Span::styled("enter", Style::new().fg(Color::Cyan)),
            Span::styled(" move  ", muted),
            Span::styled("esc", Style::new().fg(Color::Cyan)),
            Span::styled(" cancel", muted),
        ]),
        hints,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(number: usize, label: &str, pane_count: usize) -> TabInfo {
        serde_json::from_value(json!({
            "tab_id": format!("w1:t{number}"), "workspace_id": "w1", "number": number,
            "label": label, "focused": number == 1, "pane_count": pane_count,
            "agent_status": "idle",
        }))
        .unwrap()
    }

    #[test]
    fn destinations_start_with_a_new_tab_and_skip_the_current_one() {
        let tabs = [tab(1, "main", 2), tab(2, "logs", 1), tab(3, "tests", 3)];
        let found = destinations(&tabs, "w1:t2");
        let labels: Vec<_> = found.iter().map(Destination::label).collect();
        assert_eq!(labels, ["New tab", "main", "tests"]);
        assert_eq!(
            found[0],
            Destination::NewTab {
                workspace_id: "w1".into()
            }
        );
        assert_eq!(found[2].detail(), "3 panes");
        assert_eq!(destinations(&tabs[..1], "w1:t1").len(), 1);
    }

    #[test]
    fn keys_move_confirm_and_cancel() {
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(step(KeyCode::Char('j'), none, 0, 3), Step::Move(1));
        assert_eq!(step(KeyCode::Down, none, 2, 3), Step::Move(2));
        assert_eq!(step(KeyCode::Char('n'), ctrl, 0, 3), Step::Move(1));
        assert_eq!(step(KeyCode::Char('n'), none, 0, 3), Step::Ignore);
        assert_eq!(step(KeyCode::Char('k'), none, 0, 3), Step::Move(0));
        assert_eq!(step(KeyCode::Char('p'), ctrl, 2, 3), Step::Move(1));
        assert_eq!(step(KeyCode::Enter, none, 1, 3), Step::Confirm(1));
        assert_eq!(step(KeyCode::Char('3'), none, 0, 3), Step::Confirm(2));
        assert_eq!(step(KeyCode::Char('4'), none, 0, 3), Step::Ignore);
        assert_eq!(step(KeyCode::Esc, none, 0, 3), Step::Cancel);
        assert_eq!(step(KeyCode::Char('q'), none, 0, 3), Step::Cancel);
        assert_eq!(step(KeyCode::Char('c'), ctrl, 0, 3), Step::Cancel);
    }
}
