use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use unicode_truncate::UnicodeTruncateStr;
use unicode_width::UnicodeWidthStr;

use crate::model::{Item, Kind, Picker};

pub const ROW_HEIGHT: u16 = 2;

#[derive(Debug, Clone, Copy)]
pub struct Rects {
    pub header: Rect,
    pub body: Rect,
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

pub fn render(picker: &mut Picker, frame: &mut Frame) {
    let area = frame.area();
    let rects = rects(area);
    picker.visible_rows = (rects.body.height / ROW_HEIGHT) as usize;
    picker.ensure_selection_visible();

    render_header(picker, frame, rects.header);
    render_separator(frame, Rect::new(area.x, area.y + 1, area.width, 1));
    render_rows(picker, frame, rects.body);
    render_separator(frame, rects.detail);
    render_detail(picker, frame, rects.detail);
    render_footer(frame, rects.footer);
}

fn render_header(picker: &Picker, frame: &mut Frame, area: Rect) {
    let rows = picker.rows();
    let query = if picker.query.is_empty() {
        "type to search".to_string()
    } else {
        format!("{}▏", picker.query)
    };
    let left = format!(" / {query}  [{}]", picker.filter.label());
    let count = format!("{} results ", rows.len());
    let gap = area
        .width
        .saturating_sub(width(&left) as u16)
        .saturating_sub(width(&count) as u16) as usize;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                left,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" ".repeat(gap)),
            Span::styled(count, Style::default().fg(Color::DarkGray)),
        ])),
        area,
    );
}

fn render_rows(picker: &Picker, frame: &mut Frame, area: Rect) {
    let rows = picker.rows();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(" No matches").style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }
    let start = picker.scroll.min(rows.len());
    let end = rows.len().min(start + (area.height / ROW_HEIGHT) as usize);
    for (visible, index) in (start..end).enumerate() {
        render_row(
            rows[index],
            index == picker.selected,
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

fn render_row(item: &Item, selected: bool, frame: &mut Frame, area: Rect) {
    let style = if selected {
        Style::default().bg(Color::Blue).fg(Color::White)
    } else {
        Style::default()
    };
    frame.render_widget(Block::default().style(style), area);

    let badge = format!(" {:9} ", item.kind.label());
    let keys = item.keys.join(", ");
    let key_width = width(&keys).min((area.width / 3) as usize);
    let title_budget = area
        .width
        .saturating_sub(width(&badge) as u16)
        .saturating_sub(key_width as u16)
        .saturating_sub(2) as usize;
    let title = truncate_end(&item.title, title_budget);
    let gap = area
        .width
        .saturating_sub(width(&badge) as u16)
        .saturating_sub(width(&title) as u16)
        .saturating_sub(key_width as u16)
        .saturating_sub(1) as usize;
    let badge_color = match item.kind {
        Kind::NativeAction | Kind::PluginAction => Color::Magenta,
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
            Span::styled(badge, badge_style),
            Span::styled(title, style.add_modifier(Modifier::BOLD)),
            Span::styled(" ".repeat(gap), style),
            Span::styled(truncate_start(&keys, key_width), style),
            Span::styled(" ", style),
        ]))
        .style(style),
        Rect::new(area.x, area.y, area.width, 1),
    );

    frame.render_widget(
        Paragraph::new(format!(
            "            {}",
            truncate_end(&item.subtitle, area.width.saturating_sub(13) as usize)
        ))
        .style(if selected {
            style
        } else {
            Style::default().fg(Color::DarkGray)
        }),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
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
        .style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            key(" enter"),
            label(" open  "),
            key("tab"),
            label(" type  "),
            key("^A/^W/^T/⌥P/^G"),
            label(" filter  "),
            key("↑↓"),
            label(" move  "),
            key("esc"),
            label(" close"),
        ])),
        area,
    );
}

fn render_separator(frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new("─".repeat(area.width as usize)).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn key(value: &'static str) -> Span<'static> {
    Span::styled(
        value,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

fn label(value: &'static str) -> Span<'static> {
    Span::styled(value, Style::default().fg(Color::DarkGray))
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

fn truncate_start(value: &str, max: usize) -> String {
    if width(value) <= max {
        return value.into();
    }
    let Some(max) = max.checked_sub(1) else {
        return String::new();
    };
    format!("…{}", value.unicode_truncate_start(max).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_preserves_graphemes_and_width_budget() {
        assert_eq!(truncate_end("👩‍💻abc", 3), "👩‍💻…");
        assert_eq!(truncate_start("abc👩‍💻", 3), "…👩‍💻");
        assert_eq!(truncate_end("anything", 0), "");
    }
}
