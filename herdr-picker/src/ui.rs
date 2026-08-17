use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
};
use unicode_truncate::UnicodeTruncateStr;
use unicode_width::UnicodeWidthStr;

use crate::{
    Mode,
    model::{Item, Picker},
};

pub const ROW_HEIGHT: u16 = 2;

pub struct Screen<'a> {
    pub workflow_title: &'a str,
    pub step_title: &'a str,
    pub step_number: usize,
    pub step_count: usize,
    pub error: Option<&'a str>,
    pub loading: bool,
    pub spinner_frame: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct Rects {
    pub header: Rect,
    pub body: Rect,
    pub detail_separator: Rect,
    pub detail: Rect,
    pub footer: Rect,
}

pub fn rects(area: Rect) -> Rects {
    Rects {
        header: Rect::new(area.x, area.y, area.width, area.height.min(1)),
        body: Rect::new(
            area.x,
            area.y.saturating_add(2),
            area.width,
            area.height.saturating_sub(5),
        ),
        detail_separator: Rect::new(
            area.x,
            area.y.saturating_add(area.height.saturating_sub(3)),
            area.width,
            area.height.min(1),
        ),
        detail: Rect::new(
            area.x,
            area.y.saturating_add(area.height.saturating_sub(2)),
            area.width,
            area.height.min(1),
        ),
        footer: Rect::new(
            area.x,
            area.y.saturating_add(area.height.saturating_sub(1)),
            area.width,
            area.height.min(1),
        ),
    }
}

pub fn render(picker: &mut Picker, mode: Mode, screen: &Screen<'_>, frame: &mut Frame) {
    let area = frame.area();
    let rects = rects(area);
    picker.visible_rows = (rects.body.height / ROW_HEIGHT) as usize;
    picker.ensure_selection_visible();

    render_header(picker, mode, screen, frame, rects.header);
    render_separator(frame, Rect::new(area.x, area.y + 1, area.width, 1));
    render_rows(
        picker,
        screen.loading,
        screen.error,
        screen.spinner_frame,
        frame,
        rects.body,
    );
    render_separator(frame, rects.detail_separator);
    render_detail(picker, frame, rects.detail);
    render_footer(mode, screen, frame, rects.footer);
}

fn render_header(picker: &Picker, mode: Mode, screen: &Screen<'_>, frame: &mut Frame, area: Rect) {
    let focused = mode != Mode::VimNormal;
    let search_line = search_line(
        &picker.query,
        match mode {
            Mode::Direct => "type to search",
            Mode::VimNormal => "to search",
            Mode::VimSearch => "",
        },
    );
    let search_width = search_line.width();
    let context = if screen.workflow_title == screen.step_title {
        format!(
            "{} · {}/{} · {} ",
            screen.step_title,
            screen.step_number,
            screen.step_count,
            picker.len(),
        )
    } else {
        format!(
            "{} › {} · {}/{} · {} ",
            screen.workflow_title,
            screen.step_title,
            screen.step_number,
            screen.step_count,
            picker.len(),
        )
    };
    let context = if screen.loading {
        format!("{} · loading ", context.trim_end())
    } else {
        context
    };
    let context_budget = area
        .width
        .saturating_sub(search_width as u16)
        .saturating_sub(2) as usize;
    let context = truncate_start(&context, context_budget);
    let gap = (area.width as usize)
        .saturating_sub(search_width + context.width())
        .max(2);
    let mut spans = search_line.spans;
    spans.extend([
        Span::raw(" ".repeat(gap)),
        Span::styled(context, theme::muted()),
    ]);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    if let Some(position) = search_cursor_position(&picker.query, focused, area) {
        frame.set_cursor_position(position);
    }
}

fn render_rows(
    picker: &Picker,
    loading: bool,
    error: Option<&str>,
    spinner_frame: usize,
    frame: &mut Frame,
    area: Rect,
) {
    if let Some(error) = error {
        frame.render_widget(
            Paragraph::new(format!(" Error: {error}"))
                .style(Style::new().fg(Color::Red))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    if picker.is_empty() {
        frame.render_widget(
            Paragraph::new(empty_message(picker, loading)).style(theme::muted()),
            area,
        );
        return;
    }
    let start = picker.scroll.min(picker.len());
    let end = picker
        .len()
        .min(start + (area.height / ROW_HEIGHT) as usize);
    for (visible, index) in (start..end).enumerate() {
        render_row(
            picker.row(index).unwrap(),
            index == picker.selected,
            spinner_frame,
            frame,
            Rect::new(
                area.x,
                area.y + visible as u16 * ROW_HEIGHT,
                area.width,
                ROW_HEIGHT,
            ),
        );
    }
}

fn empty_message(picker: &Picker, loading: bool) -> &'static str {
    if loading {
        " Loading…"
    } else if picker.query.trim().is_empty() {
        " No items"
    } else {
        " No matches"
    }
}

fn render_row(item: &Item, selected: bool, spinner_frame: usize, frame: &mut Frame, area: Rect) {
    let style = if selected {
        theme::selection()
    } else {
        Style::default()
    };
    frame.render_widget(Block::default().style(style), area);

    let badge = (!item.badge.is_empty()).then(|| format!(" {} ", item.badge));
    let indicator = if item.spinning {
        ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][spinner_frame % 10]
    } else {
        &item.indicator
    };
    let indicator = (!indicator.is_empty()).then(|| format!(" {indicator} "));
    let left_padding = usize::from(badge.is_none() && indicator.is_none());
    let prefix_width = left_padding
        + badge.as_deref().map_or(0, UnicodeWidthStr::width)
        + indicator.as_deref().map_or(0, UnicodeWidthStr::width);
    let title_budget = area
        .width
        .saturating_sub(prefix_width as u16)
        .saturating_sub(1) as usize;
    let title = truncate_end(&item.title, title_budget);
    let gap = area
        .width
        .saturating_sub(prefix_width as u16)
        .saturating_sub(title.width() as u16) as usize;
    let badge_style = if selected {
        style.add_modifier(Modifier::BOLD)
    } else {
        theme::accent()
    };
    let indicator_style = if selected {
        style.add_modifier(Modifier::BOLD)
    } else {
        tone_style(item.tone)
    };
    let mut spans = vec![Span::raw(" ".repeat(left_padding))];
    if let Some(indicator) = indicator {
        spans.push(Span::styled(indicator, indicator_style));
    }
    if let Some(badge) = badge {
        spans.push(Span::styled(badge, badge_style));
    }
    spans.extend([
        Span::styled(title, style.add_modifier(Modifier::BOLD)),
        Span::styled(" ".repeat(gap), style),
    ]);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(style),
        Rect::new(area.x, area.y, area.width, 1),
    );

    frame.render_widget(
        Paragraph::new(format!(
            "{}{}",
            " ".repeat(prefix_width),
            truncate_end(
                &item.subtitle,
                area.width.saturating_sub(prefix_width as u16) as usize
            )
        ))
        .style(if selected { style } else { theme::muted() }),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
}

fn render_detail(picker: &Picker, frame: &mut Frame, area: Rect) {
    let detail = format!(
        " {}",
        picker
            .selected_item()
            .map(|item| item.detail.as_str())
            .unwrap_or("")
    );
    frame.render_widget(
        Paragraph::new(truncate_end(&detail, area.width.saturating_sub(1) as usize))
            .style(theme::muted()),
        area,
    );
}

fn tone_style(tone: Option<crate::model::Tone>) -> Style {
    use crate::model::Tone;
    match tone {
        Some(Tone::Muted) => theme::muted(),
        Some(Tone::Accent) => theme::accent(),
        Some(Tone::Success) => Style::new().fg(Color::Green),
        Some(Tone::Warning) => Style::new().fg(Color::Yellow),
        Some(Tone::Danger) => Style::new().fg(Color::Red),
        None => Style::default(),
    }
}

fn render_footer(mode: Mode, screen: &Screen<'_>, frame: &mut Frame, area: Rect) {
    let hints = [
        (
            "enter",
            if screen.step_number == screen.step_count {
                "run"
            } else {
                "next"
            },
        ),
        (if mode == Mode::VimNormal { "/" } else { "type" }, "search"),
        (
            if mode == Mode::VimNormal {
                "j/k"
            } else {
                "↑↓"
            },
            "move",
        ),
        ("esc", escape_hint(mode, screen.step_number)),
    ];
    let button = back_button_rect(area, screen.step_number);
    let hints_area = Rect::new(
        area.x,
        area.y,
        button.x.saturating_sub(area.x).saturating_sub(1),
        area.height,
    );
    frame.render_widget(Paragraph::new(key_hints(&hints)), hints_area);
    frame.render_widget(
        Paragraph::new(format!(" {} ", back_button_label(screen.step_number)))
            .style(theme::button()),
        button,
    );
}

pub fn back_button_rect(area: Rect, step_number: usize) -> Rect {
    let width = back_button_label(step_number).width() as u16 + 2;
    Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y,
        area.width.min(width),
        area.height.min(1),
    )
}

fn back_button_label(step_number: usize) -> &'static str {
    if step_number > 1 { "Back" } else { "Close" }
}

fn search_line<'a>(query: &'a str, placeholder: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled("/ ", theme::accent()),
        if query.is_empty() {
            Span::styled(placeholder, theme::muted())
        } else {
            Span::raw(query)
        },
    ])
}

fn search_cursor_position(query: &str, focused: bool, area: Rect) -> Option<(u16, u16)> {
    if !focused || area.width == 0 || area.height == 0 {
        return None;
    }
    let offset = 2usize
        .saturating_add(Line::from(query).width())
        .min(usize::from(area.width.saturating_sub(1))) as u16;
    Some((area.x.saturating_add(offset), area.y))
}

fn render_separator(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Line::styled("─".repeat(area.width as usize), theme::muted()),
        area,
    );
}

fn key_hints<'a>(pairs: &[(&'a str, &'a str)]) -> Line<'a> {
    pairs
        .iter()
        .enumerate()
        .flat_map(|(index, (key, label))| {
            [
                Span::raw(if index == 0 { "" } else { "  " }),
                Span::styled(*key, theme::accent()),
                Span::styled(format!(" {label}"), theme::muted()),
            ]
        })
        .collect()
}

fn escape_hint(mode: Mode, step_number: usize) -> &'static str {
    if mode == Mode::VimSearch {
        "normal"
    } else if step_number > 1 {
        "back"
    } else {
        "close"
    }
}

mod theme {
    use ratatui::style::{Color, Modifier, Style};

    pub fn accent() -> Style {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    }

    pub fn muted() -> Style {
        Style::new().fg(Color::DarkGray)
    }

    pub fn selection() -> Style {
        Style::new().bg(Color::Blue).fg(Color::White)
    }

    pub fn button() -> Style {
        Style::new()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    }
}

fn truncate_end(value: &str, max: usize) -> String {
    if value.width() <= max {
        return value.into();
    }
    let Some(max) = max.checked_sub(1) else {
        return String::new();
    };
    format!("{}…", value.unicode_truncate(max).0)
}

fn truncate_start(value: &str, max: usize) -> String {
    if value.width() <= max {
        return value.into();
    }
    let Some(max) = max.checked_sub(1) else {
        return String::new();
    };
    let truncated = value.unicode_truncate_start(max).0;
    format!("…{truncated}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    #[test]
    fn truncation_preserves_graphemes_and_width_budget() {
        assert_eq!(truncate_end("👩‍💻abc", 3), "👩‍💻…");
        assert_eq!(truncate_end("anything", 0), "");
        assert!(truncate_start("a long title", 6).width() <= 6);
    }

    #[test]
    fn local_chrome_matches_picker_modes() {
        assert_eq!(
            search_line("agent", "type to search").to_string(),
            "/ agent"
        );
        assert_eq!(
            search_cursor_position("agent", true, Rect::new(4, 2, 20, 1)),
            Some((11, 2))
        );
        assert_eq!(
            key_hints(&[("enter", "open"), ("esc", "close")]).to_string(),
            "enter open  esc close"
        );
        assert_eq!(escape_hint(Mode::VimSearch, 2), "normal");
        assert_eq!(escape_hint(Mode::Direct, 2), "back");
        assert_eq!(escape_hint(Mode::Direct, 1), "close");
        assert_eq!(
            back_button_rect(Rect::new(0, 2, 20, 1), 1),
            Rect::new(13, 2, 7, 1)
        );
    }

    #[test]
    fn empty_rows_distinguish_items_from_matches() {
        let mut picker = Picker::new(Vec::new(), true);
        assert_eq!(empty_message(&picker, true), " Loading…");
        assert_eq!(empty_message(&picker, false), " No items");
        picker.query = "missing".into();
        assert_eq!(empty_message(&picker, false), " No matches");
    }

    #[test]
    fn provider_errors_wrap_in_the_body() {
        let picker = Picker::new(Vec::new(), true);
        let mut terminal = Terminal::new(TestBackend::new(24, 3)).unwrap();
        terminal
            .draw(|frame| {
                render_rows(
                    &picker,
                    false,
                    Some("invalid snapshot\nsource diagnostic"),
                    0,
                    frame,
                    frame.area(),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..3)
            .flat_map(|row| (0..24).map(move |column| buffer.cell((column, row)).unwrap().symbol()))
            .collect::<String>();

        assert!(rendered.contains("source diagnostic"));
    }

    #[test]
    fn detail_separator_has_its_own_row() {
        let rects = rects(Rect::new(0, 0, 80, 20));
        assert_eq!(rects.detail_separator.y + 1, rects.detail.y);
    }

    #[test]
    fn generic_badge_sets_row_indentation() {
        let item = Item {
            id: "one".into(),
            title: "title".into(),
            subtitle: "subtitle".into(),
            badge: "rust".into(),
            value: json!(null),
            ..Item::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(20, ROW_HEIGHT)).unwrap();
        terminal
            .draw(|frame| render_row(&item, false, 0, frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer.cell((1, 0)).unwrap().symbol(), "r");
        assert_eq!(buffer.cell((6, 0)).unwrap().symbol(), "t");
        assert_eq!(buffer.cell((6, 1)).unwrap().symbol(), "s");
    }
}
