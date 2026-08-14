use herdr_ratatui::{SearchLine, Separator, key_hints, theme};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use unicode_truncate::UnicodeTruncateStr;
use unicode_width::UnicodeWidthStr;

use crate::{
    Mode,
    model::{Filter, Item, Kind, Picker},
};
use herdr_client::AgentStatus;

pub const ROW_HEIGHT: u16 = 2;

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

pub fn render(picker: &mut Picker, mode: Mode, spinner_frame: usize, frame: &mut Frame) {
    let area = frame.area();
    let rects = rects(area);
    picker.visible_rows = (rects.body.height / ROW_HEIGHT) as usize;
    picker.ensure_selection_visible();

    render_header(picker, mode, frame, rects.header);
    frame.render_widget(Separator, Rect::new(area.x, area.y + 1, area.width, 1));
    render_rows(picker, spinner_frame, frame, rects.body);
    frame.render_widget(Separator, rects.detail_separator);
    render_detail(picker, frame, rects.detail);
    render_footer(mode, frame, rects.footer);
}

fn render_header(picker: &Picker, mode: Mode, frame: &mut Frame, area: Rect) {
    let search = SearchLine {
        query: &picker.query,
        placeholder: match mode {
            Mode::Direct => "type to search",
            Mode::VimNormal => "to search",
            Mode::VimSearch => "",
        },
        focused: mode != Mode::VimNormal,
    };
    let filter = format!("[{}]", picker.filter.label());
    let count = format!("{} results ", picker.len());
    let gap = (area.width as usize)
        .saturating_sub(search.line().width() + 2 + width(&filter))
        .saturating_sub(width(&count));
    let mut spans = search.line().spans;
    spans.extend([
        Span::raw("  "),
        Span::styled(filter, theme::accent()),
        Span::raw(" ".repeat(gap)),
        Span::styled(count, theme::muted()),
    ]);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    if let Some(position) = search.cursor_position(area) {
        frame.set_cursor_position(position);
    }
}

fn render_rows(picker: &Picker, spinner_frame: usize, frame: &mut Frame, area: Rect) {
    if picker.is_empty() {
        frame.render_widget(Paragraph::new(" No matches").style(theme::muted()), area);
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
            picker.filter == Filter::All,
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

fn render_row(
    item: &Item,
    selected: bool,
    show_badge: bool,
    spinner_frame: usize,
    frame: &mut Frame,
    area: Rect,
) {
    let style = if selected {
        theme::selection()
    } else {
        Style::default()
    };
    frame.render_widget(Block::default().style(style), area);

    let badge = show_badge.then(|| format!(" {:9} ", item.kind.label()));
    let glyph = (item.kind == Kind::Agent)
        .then(|| agent_status_glyph(item.agent_status.as_ref(), spinner_frame));
    let left_padding = usize::from(!show_badge);
    let prefix_width = left_padding
        + badge.as_deref().map_or(0, width)
        + glyph.map_or(0, |(glyph, _)| width(glyph) + 1);
    let title_budget = area
        .width
        .saturating_sub(prefix_width as u16)
        .saturating_sub(1) as usize;
    let title = truncate_end(&item.title, title_budget);
    let gap = area
        .width
        .saturating_sub(prefix_width as u16)
        .saturating_sub(width(&title) as u16) as usize;
    let badge_color = match item.kind {
        Kind::Workspace => Color::Cyan,
        Kind::Tab => Color::Green,
        Kind::Pane => Color::Yellow,
        Kind::Agent => Color::LightRed,
    };
    let badge_style = if selected {
        style.add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(badge_color)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" ".repeat(left_padding)),
            Span::styled(badge.unwrap_or_default(), badge_style),
            glyph.map_or_else(
                || Span::raw(""),
                |(glyph, color)| {
                    Span::styled(
                        format!("{glyph} "),
                        if selected {
                            style.fg(color)
                        } else {
                            Style::default().fg(color)
                        },
                    )
                },
            ),
            Span::styled(title, style.add_modifier(Modifier::BOLD)),
            Span::styled(" ".repeat(gap), style),
        ]))
        .style(style),
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

fn agent_status_glyph(status: Option<&AgentStatus>, spinner_frame: usize) -> (&'static str, Color) {
    match status.map(AgentStatus::as_str) {
        Some(AgentStatus::BLOCKED) => ("◉", Color::Red),
        Some(AgentStatus::DONE) => ("●", Color::Cyan),
        Some(AgentStatus::WORKING) => (
            ["⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "⠋", "⠙", "⠹", "⠸"][spinner_frame % 10],
            Color::Yellow,
        ),
        Some(AgentStatus::IDLE) => ("✓", Color::Green),
        _ => ("○", Color::DarkGray),
    }
}

fn render_detail(picker: &Picker, frame: &mut Frame, area: Rect) {
    let detail = picker
        .selected_item()
        .map(|item| item.detail.as_str())
        .unwrap_or("");
    frame.render_widget(
        Paragraph::new(format!(
            " {}",
            truncate_end(detail, area.width.saturating_sub(2) as usize)
        ))
        .style(theme::muted()),
        area,
    );
}

fn render_footer(mode: Mode, frame: &mut Frame, area: Rect) {
    let movement = if mode == Mode::VimNormal {
        "j/k"
    } else {
        "↑↓"
    };
    let escape = if mode == Mode::VimSearch {
        "normal"
    } else {
        "close"
    };
    frame.render_widget(
        Paragraph::new(key_hints(&[
            ("enter", "open"),
            (if mode == Mode::VimNormal { "/" } else { "type" }, "search"),
            ("^W/^T/⌥P/^G", "filter"),
            (movement, "move"),
            ("esc", escape),
        ])),
        area,
    );
}

fn width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

fn truncate_end(value: &str, max: usize) -> String {
    if width(value) <= max {
        return value.into();
    }
    let Some(max) = max.checked_sub(1) else {
        return String::new();
    };
    format!("{}…", value.unicode_truncate(max).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Dispatch;
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    #[test]
    fn truncation_preserves_graphemes_and_width_budget() {
        assert_eq!(truncate_end("👩‍💻abc", 3), "👩‍💻…");
        assert_eq!(truncate_end("anything", 0), "");
    }

    #[test]
    fn detail_separator_has_its_own_row() {
        let rects = rects(Rect::new(0, 0, 80, 20));
        assert_eq!(rects.detail_separator.y + 1, rects.detail.y);
    }

    #[test]
    fn agent_status_glyphs_cover_known_and_future_statuses() {
        for (status, frame, expected) in [
            ("blocked", 1, ("◉", Color::Red)),
            ("done", 1, ("●", Color::Cyan)),
            ("working", 0, ("⠼", Color::Yellow)),
            ("working", 1, ("⠴", Color::Yellow)),
            ("idle", 1, ("✓", Color::Green)),
            ("future", 1, ("○", Color::DarkGray)),
        ] {
            assert_eq!(
                agent_status_glyph(Some(&status.into()), frame),
                expected,
                "{status} frame {frame}"
            );
        }
    }

    #[test]
    fn scoped_agent_rows_pad_before_the_status_glyph() {
        let item = Item {
            kind: Kind::Agent,
            agent_status: Some("done".into()),
            title: "title".into(),
            subtitle: "subtitle".into(),
            detail: String::new(),
            dispatch: Dispatch::new("test", json!({})),
        };
        let mut terminal = Terminal::new(TestBackend::new(20, ROW_HEIGHT)).unwrap();
        terminal
            .draw(|frame| render_row(&item, false, false, 0, frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((1, 0)).unwrap().symbol(), "●");
        assert_eq!(buffer.cell((3, 0)).unwrap().symbol(), "t");
        assert_eq!(buffer.cell((3, 1)).unwrap().symbol(), "s");
    }
}
