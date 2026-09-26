// The group picker popup: type to filter or name a new group, then Enter.
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState},
};

use crate::state::{MAX_GROUP_LEN, normalize_group};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Group(String),
    Create(String),
    Remove,
}

/// Picker rows for `query`. A typed name that matches no group exactly is
/// offered for creation first; clearing is offered while the query is empty.
pub fn options(groups: &[String], query: &str, current: Option<&str>) -> Vec<Choice> {
    let typed = normalize_group(query);
    let needle = typed.as_deref().unwrap_or_default().to_lowercase();
    let mut options = Vec::new();
    if let Some(typed) = typed.as_ref().filter(|typed| !groups.contains(typed)) {
        options.push(Choice::Create(typed.clone()));
    }
    options.extend(
        groups
            .iter()
            .filter(|group| group.to_lowercase().contains(&needle))
            .cloned()
            .map(Choice::Group),
    );
    if typed.is_none() && current.is_some() {
        options.push(Choice::Remove);
    }
    options
}

pub fn choose(
    terminal: &mut ratatui::DefaultTerminal,
    title: &str,
    groups: &[String],
    current: Option<&str>,
) -> Result<Option<Choice>> {
    let mut query = String::new();
    let mut selected = 0;
    loop {
        let rows = options(groups, &query, current);
        selected = selected.min(rows.len().saturating_sub(1));
        terminal.draw(|frame| render(frame, title, &query, &rows, selected, current))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match step(key.code, key.modifiers, selected, rows.len()) {
            Step::Move(next) => selected = next,
            Step::Confirm => return Ok(rows.get(selected).cloned()),
            Step::Cancel => return Ok(None),
            Step::Type(ch) if query.chars().count() < MAX_GROUP_LEN => {
                query.push(ch);
                selected = 0;
            }
            Step::Backspace => {
                query.pop();
                selected = 0;
            }
            Step::Type(_) | Step::Ignore => {}
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Move(usize),
    Confirm,
    Cancel,
    Type(char),
    Backspace,
    Ignore,
}

fn step(code: KeyCode, modifiers: KeyModifiers, selected: usize, len: usize) -> Step {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let last = len.saturating_sub(1);
    match code {
        KeyCode::Esc => Step::Cancel,
        KeyCode::Char('c') if ctrl => Step::Cancel,
        KeyCode::Enter => Step::Confirm,
        KeyCode::Down | KeyCode::Tab => Step::Move((selected + 1).min(last)),
        KeyCode::Char('n') if ctrl => Step::Move((selected + 1).min(last)),
        KeyCode::Up | KeyCode::BackTab => Step::Move(selected.saturating_sub(1)),
        KeyCode::Char('p') if ctrl => Step::Move(selected.saturating_sub(1)),
        KeyCode::Backspace => Step::Backspace,
        KeyCode::Char(ch) if !ctrl => Step::Type(ch),
        _ => Step::Ignore,
    }
}

fn render(
    frame: &mut Frame<'_>,
    title: &str,
    query: &str,
    rows: &[Choice],
    selected: usize,
    current: Option<&str>,
) {
    let muted = Style::new().fg(Color::DarkGray);
    let accent = Style::new().fg(Color::Cyan);
    let [title_area, input, body, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(
        Line::styled(title.to_owned(), Style::new().add_modifier(Modifier::BOLD)),
        title_area,
    );
    frame.render_widget(
        Line::from(vec![
            Span::styled("› ", accent),
            Span::raw(query.to_owned()),
            Span::styled("▏", accent),
        ]),
        input,
    );
    let items = rows.iter().map(|choice| {
        ListItem::new(match choice {
            Choice::Create(name) => Line::from(vec![
                Span::styled("+ ", accent),
                Span::raw(format!("New group “{name}”")),
            ]),
            Choice::Group(name) if Some(name.as_str()) == current => Line::from(vec![
                Span::raw(format!("  {name}")),
                Span::styled("  current", muted),
            ]),
            Choice::Group(name) => Line::raw(format!("  {name}")),
            Choice::Remove => Line::styled("  Remove from group", muted),
        })
    });
    let list = List::new(items).highlight_style(Style::new().bg(Color::Blue).fg(Color::White));
    let mut state = ListState::default().with_selected((!rows.is_empty()).then_some(selected));
    frame.render_stateful_widget(list, body, &mut state);
    frame.render_widget(
        Line::from(vec![
            Span::styled("type", accent),
            Span::styled(" filter/name  ", muted),
            Span::styled("↑↓", accent),
            Span::styled(" move  ", muted),
            Span::styled("enter", accent),
            Span::styled(" set  ", muted),
            Span::styled("esc", accent),
            Span::styled(" cancel", muted),
        ]),
        hints,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> Vec<String> {
        vec!["infra".into(), "Play".into(), "work".into()]
    }

    #[test]
    fn empty_query_lists_groups_and_offers_removal_only_when_assigned() {
        let all = [
            Choice::Group("infra".into()),
            Choice::Group("Play".into()),
            Choice::Group("work".into()),
        ];
        assert_eq!(options(&groups(), "", None), all);
        let mut assigned = all.to_vec();
        assigned.push(Choice::Remove);
        assert_eq!(options(&groups(), "  ", Some("work")), assigned);
    }

    #[test]
    fn typing_filters_and_offers_a_new_group_unless_it_exists() {
        assert_eq!(
            options(&groups(), "pl", Some("work")),
            [Choice::Create("pl".into()), Choice::Group("Play".into())]
        );
        assert_eq!(
            options(&groups(), " work ", None),
            [Choice::Group("work".into())]
        );
        assert_eq!(options(&[], "ops", None), [Choice::Create("ops".into())]);
    }

    #[test]
    fn keys_type_move_confirm_and_cancel() {
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(step(KeyCode::Char('j'), none, 0, 3), Step::Type('j'));
        assert_eq!(step(KeyCode::Down, none, 2, 3), Step::Move(2));
        assert_eq!(step(KeyCode::Char('n'), ctrl, 0, 3), Step::Move(1));
        assert_eq!(step(KeyCode::Char('p'), ctrl, 2, 3), Step::Move(1));
        assert_eq!(step(KeyCode::Up, none, 0, 3), Step::Move(0));
        assert_eq!(step(KeyCode::Backspace, none, 0, 3), Step::Backspace);
        assert_eq!(step(KeyCode::Enter, none, 1, 3), Step::Confirm);
        assert_eq!(step(KeyCode::Esc, none, 0, 3), Step::Cancel);
        assert_eq!(step(KeyCode::Char('c'), ctrl, 0, 3), Step::Cancel);
    }
}
