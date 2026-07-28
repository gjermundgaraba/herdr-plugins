//! Rendering, copied from herdr src/ui/navigator.rs plus its helpers in
//! src/ui/{status,text,scrollbar,widgets}.rs (Apache-2.0,
//! github.com/ogulcancelik/herdr).
//!
//! Herdr draws the popup border (accent + panel_bg + title) around the
//! plugin's terminal, so the whole frame area here IS the native modal's
//! inner rect and `render_panel_shell` is not copied.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::config::Palette;
use crate::model::{AgentRow, AgentState, Picker, StateFilter};

/// Each agent takes two lines: workspace + agent·state, then the task line.
pub const ROW_HEIGHT: u16 = 2;

// --- geometry (from herdr src/app/input/overlays.rs) -----------------------

#[derive(Debug, Clone, Copy)]
pub struct NavRects {
    pub search: Rect,
    pub body: Rect,
    pub detail: Rect,
    pub footer: Rect,
}

pub fn nav_rects(inner: Rect) -> NavRects {
    let search = Rect::new(inner.x, inner.y, inner.width, inner.height.min(1));
    let body = if inner.height <= 4 {
        Rect::default()
    } else {
        Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(4),
        )
    };
    let detail = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(2),
        inner.width,
        inner.height.min(1),
    );
    let footer = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        inner.height.min(1),
    );
    NavRects {
        search,
        body,
        detail,
        footer,
    }
}

// --- spinner + icons (from herdr src/ui.rs and src/ui/status.rs) -----------

// Braille spinner frames — smooth rotation. Native ticks per frame at ~60fps
// and divides by 8; the plugin ticks once per 125ms poll, so no division.
const SPINNERS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn spinner_frame(tick: u32) -> &'static str {
    SPINNERS[tick as usize % SPINNERS.len()]
}

fn agent_icon(state: AgentState, seen: bool, tick: u32, p: &Palette) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("◉", Style::default().fg(p.red)),
        (AgentState::Working, _) => (spinner_frame(tick), Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("✓", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("○", Style::default().fg(p.overlay0)),
    }
}

fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Blocked, _) => p.red,
        (AgentState::Working, _) => p.yellow,
        (AgentState::Idle, false) => p.teal,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
}

fn panel_contrast_fg(p: &Palette) -> Color {
    match p.panel_bg {
        Color::Reset => p.surface_dim,
        color => color,
    }
}

// --- text helpers (from herdr src/ui/text.rs) ------------------------------

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn display_width_u16(text: &str) -> u16 {
    display_width(text).min(u16::MAX as usize) as u16
}

fn truncate_end(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let prefix = take_prefix_width(text, max_width.saturating_sub(1));
    format!("{prefix}…")
}

fn middle_elide(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let content_width = max_width.saturating_sub(1);
    let left_width = content_width / 2;
    let right_width = content_width.saturating_sub(left_width);
    let prefix = take_prefix_width(text, left_width);
    let suffix = take_suffix_width(text, right_width);
    format!("{prefix}…{suffix}")
}

fn take_prefix_width(text: &str, max_width: usize) -> String {
    let mut output = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        output.push(ch);
        width += ch_width;
    }
    output
}

fn take_suffix_width(text: &str, max_width: usize) -> String {
    let mut output = Vec::new();
    let mut width = 0usize;
    for ch in text.chars().rev() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        output.push(ch);
        width += ch_width;
    }
    output.into_iter().rev().collect()
}

// --- scrollbar (from herdr src/ui/scrollbar.rs) ----------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollMetrics {
    offset_from_bottom: usize,
    max_offset_from_bottom: usize,
    viewport_rows: usize,
}

fn should_show_scrollbar(metrics: ScrollMetrics) -> bool {
    metrics.max_offset_from_bottom > 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollbarThumb {
    top: u16,
    len: u16,
}

fn scrollbar_thumb(metrics: ScrollMetrics, track: Rect) -> Option<ScrollbarThumb> {
    if metrics.max_offset_from_bottom == 0 || track.height == 0 {
        return None;
    }

    let track_height = track.height as usize;
    let total_rows = metrics.max_offset_from_bottom + metrics.viewport_rows;
    if total_rows == 0 {
        return None;
    }

    let thumb_len = ((metrics.viewport_rows * track_height) as f32 / total_rows as f32)
        .round()
        .max(1.0)
        .min(track_height as f32) as usize;
    let max_thumb_top = track_height.saturating_sub(thumb_len);
    let scrolled_from_top = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let thumb_top = if max_thumb_top == 0 || metrics.max_offset_from_bottom == 0 {
        0
    } else {
        ((scrolled_from_top * max_thumb_top) as f32 / metrics.max_offset_from_bottom as f32)
            .round()
            .clamp(0.0, max_thumb_top as f32) as usize
    };

    Some(ScrollbarThumb {
        top: track.y + thumb_top as u16,
        len: thumb_len as u16,
    })
}

fn render_scrollbar(
    frame: &mut Frame,
    metrics: ScrollMetrics,
    track: Rect,
    track_color: Color,
    thumb_color: Color,
    thumb_symbol: &str,
) {
    if metrics.max_offset_from_bottom == 0 {
        return;
    }

    let Some(thumb) = scrollbar_thumb(metrics, track) else {
        return;
    };

    let buf = frame.buffer_mut();
    for y in track.y..track.y + track.height {
        let cell = &mut buf[(track.x, y)];
        cell.set_symbol("▕");
        cell.set_style(Style::default().fg(track_color));
    }
    for y in thumb.top..thumb.top + thumb.len {
        let cell = &mut buf[(track.x, y)];
        cell.set_symbol(thumb_symbol);
        cell.set_style(Style::default().fg(thumb_color));
    }
}

// --- navigator rendering (from herdr src/ui/navigator.rs) ------------------

pub fn render(picker: &mut Picker, p: &Palette, tick: u32, frame: &mut Frame) {
    let inner = frame.area();
    let rects = nav_rects(inner);
    picker.visible_rows = (rects.body.height / ROW_HEIGHT) as usize;
    picker.ensure_selection_visible();

    frame.render_widget(
        Block::default().style(Style::default().bg(p.panel_bg)),
        inner,
    );
    render_search(picker, p, tick, frame, rects.search);

    if rects.body.height > 0 {
        let rows = picker.rows();
        render_separator(
            frame,
            Rect::new(inner.x, rects.search.y + 1, inner.width, 1),
            p,
        );
        render_rows(picker, p, tick, &rows, frame, rects.body);
        render_list_scrollbar(picker, p, rows.len(), frame, rects.body);
        render_detail(&rows, picker.selected, p, frame, rects.detail);
    } else {
        render_detail(&[], 0, p, frame, rects.detail);
    }
    render_footer(picker, p, frame, rects.footer);
}

fn render_search(picker: &Picker, p: &Palette, tick: u32, frame: &mut Frame, area: Rect) {
    let focus_style = if picker.search_focused {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    let mut spans = vec![Span::styled(" / ", focus_style)];
    let query = picker.query.trim();
    if picker.state_filters.is_empty() {
        if query.is_empty() {
            spans.push(Span::styled(
                "search agents",
                Style::default().fg(p.overlay0),
            ));
        } else {
            spans.push(Span::styled(query.to_string(), Style::default().fg(p.text)));
        }
    } else {
        // Multi-select deviation: one chip per active filter.
        for (chip_idx, filter) in picker.state_filters.iter().enumerate() {
            if chip_idx > 0 {
                spans.push(Span::raw(" "));
            }
            let (state, seen, label) = match filter {
                StateFilter::Blocked => (AgentState::Blocked, true, "blocked"),
                StateFilter::Working => (AgentState::Working, true, "working"),
                StateFilter::Idle => (AgentState::Idle, true, "idle"),
                StateFilter::Done => (AgentState::Idle, false, "done"),
            };
            push_state_chip(&mut spans, state, seen, tick, label, p);
        }
    }
    // Right side: live inbox summary instead of the native pane count.
    let summary = picker.activity_summary();
    spans.push(Span::styled(
        format!(
            "{summary:>width$}",
            width = area.width.saturating_sub(16) as usize
        ),
        Style::default().fg(p.overlay0),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn push_state_chip(
    spans: &mut Vec<Span<'static>>,
    state: AgentState,
    seen: bool,
    tick: u32,
    label: &'static str,
    p: &Palette,
) {
    let (icon, icon_style) = agent_icon(state, seen, tick, p);
    spans.push(Span::styled(icon, icon_style.add_modifier(Modifier::BOLD)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        label,
        Style::default()
            .fg(state_label_color(state, seen, p))
            .add_modifier(Modifier::BOLD),
    ));
}

fn render_separator(frame: &mut Frame, area: Rect, p: &Palette) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(p.surface1)),
        area,
    );
}

fn render_rows(
    picker: &Picker,
    p: &Palette,
    tick: u32,
    rows: &[AgentRow],
    frame: &mut Frame,
    body: Rect,
) {
    let start = picker.scroll.min(rows.len());
    let end = rows
        .len()
        .min(start.saturating_add((body.height / ROW_HEIGHT) as usize));
    for (visible_idx, idx) in (start..end).enumerate() {
        let y = body.y + visible_idx as u16 * ROW_HEIGHT;
        render_row(p, tick, frame, body, y, &rows[idx], idx == picker.selected);
    }
}

/// Two lines per agent:
///   ` ◆ ⠼ workspace              agent · state`
///   `     the task line`
fn render_row(
    p: &Palette,
    tick: u32,
    frame: &mut Frame,
    body: Rect,
    y: u16,
    row: &AgentRow,
    selected: bool,
) {
    let top = Rect::new(body.x, y, body.width, 1);
    let bottom = Rect::new(body.x, y + 1, body.width, 1);
    frame.render_widget(Clear, top);
    frame.render_widget(Clear, bottom);
    let base_style = if selected {
        Style::default().bg(p.accent).fg(panel_contrast_fg(p))
    } else {
        Style::default().bg(p.panel_bg).fg(p.text)
    };
    let (status_icon, status_style) = agent_icon(row.status, row.seen, tick, p);
    let status_style = if selected {
        base_style.add_modifier(Modifier::BOLD)
    } else {
        status_style.bg(p.panel_bg)
    };
    let workspace_style = if selected {
        base_style.add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.mauve).bg(p.panel_bg)
    };
    let title_style = if selected {
        base_style
    } else if row.is_current {
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0).bg(p.panel_bg)
    };

    let current = if row.is_current { "◆" } else { " " };
    let gutter = format!(" {current} ");
    let gutter_style = if selected {
        base_style
    } else if row.is_current {
        Style::default().fg(p.accent).bg(p.panel_bg)
    } else {
        Style::default().fg(p.overlay0).bg(p.panel_bg)
    };
    // Indent both lines identically: gutter (3) + icon (1) + space (1).
    let indent = display_width_u16(&gutter) + 2;

    let star = if row.pinned { "★ " } else { "" };
    let meta_width = metadata_width(top.width);
    let workspace_budget = top
        .width
        .saturating_sub(meta_width)
        .saturating_sub(indent)
        .saturating_sub(display_width_u16(star))
        .saturating_sub(1) as usize;
    let star_style = if selected {
        base_style.add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.yellow).bg(p.panel_bg)
    };
    let spans = vec![
        Span::styled(gutter, gutter_style),
        Span::styled(status_icon, status_style),
        Span::raw(" "),
        Span::styled(star, star_style),
        Span::styled(
            truncate_end(&row.workspace, workspace_budget),
            workspace_style,
        ),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)).style(base_style), top);

    if meta_width > 0 {
        let meta_rect = Rect::new(
            top.x + top.width.saturating_sub(meta_width),
            top.y,
            meta_width,
            1,
        );
        let meta = truncate_end(&row.meta, meta_width.saturating_sub(2) as usize);
        let meta_style = if selected {
            base_style
        } else {
            Style::default()
                .fg(state_label_color(row.status, row.seen, p))
                .bg(p.panel_bg)
        };
        frame.render_widget(
            Paragraph::new(format!(" {meta}")).style(meta_style),
            meta_rect,
        );
    }

    // Line 2 leads with the working directory (capped to a third of the row
    // so it cannot eat the task line), then the task line.
    let path_budget = (bottom.width / 3) as usize;
    let path = if path_budget < 8 {
        String::new()
    } else {
        middle_elide(&row.path, path_budget)
    };
    let dim_style = if selected {
        base_style
    } else {
        Style::default().fg(p.overlay0).bg(p.panel_bg)
    };
    let mut spans = vec![Span::styled(" ".repeat(indent as usize), base_style)];
    let mut title_budget = bottom.width.saturating_sub(indent).saturating_sub(2) as usize;
    if !path.is_empty() {
        title_budget = title_budget
            .saturating_sub(display_width(&path))
            .saturating_sub(3);
        spans.push(Span::styled(path, dim_style));
        spans.push(Span::styled(" · ", dim_style));
    }
    spans.push(Span::styled(
        truncate_end(&row.label, title_budget),
        title_style,
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(base_style), bottom);
}

fn render_list_scrollbar(
    picker: &Picker,
    p: &Palette,
    line_count: usize,
    frame: &mut Frame,
    body: Rect,
) {
    if body.width <= 1 || body.height == 0 {
        return;
    }
    let viewport = (body.height / ROW_HEIGHT) as usize;
    if line_count <= viewport {
        return;
    }
    let metrics = ScrollMetrics {
        viewport_rows: viewport,
        offset_from_bottom: line_count
            .saturating_sub(viewport)
            .saturating_sub(picker.scroll),
        max_offset_from_bottom: line_count.saturating_sub(viewport),
    };
    if !should_show_scrollbar(metrics) {
        return;
    }
    let track = Rect::new(body.x + body.width - 1, body.y, 1, body.height);
    render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
}

/// Native breakpoints: the metadata column carries "agent · state" at most.
fn metadata_width(width: u16) -> u16 {
    if width >= 90 {
        28
    } else if width >= 68 {
        20
    } else if width >= 52 {
        14
    } else {
        0
    }
}

fn render_detail(rows: &[AgentRow], selected: usize, p: &Palette, frame: &mut Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    render_separator(frame, area, p);
    let detail = rows
        .get(selected)
        .map(|row| row.detail.clone())
        .unwrap_or_default();
    if detail.is_empty() {
        return;
    }
    let text = middle_elide(&detail, area.width.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(format!(" {text}")).style(Style::default().fg(p.overlay0)),
        area,
    );
}

fn render_footer(picker: &Picker, p: &Palette, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let key = Style::default().fg(p.accent).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(p.overlay0);
    let line = if picker.search_focused {
        Line::from(vec![
            Span::styled(" enter", key),
            Span::styled(" switch  ", dim),
            Span::styled("↑↓", key),
            Span::styled(" move  ", dim),
            Span::styled("ctrl+u", key),
            Span::styled(" clear  ", dim),
            Span::styled("esc", key),
            Span::styled(" back", dim),
        ])
    } else {
        Line::from(vec![
            Span::styled(" enter", key),
            Span::styled(" switch  ", dim),
            Span::styled("/", key),
            Span::styled(" search  ", dim),
            Span::styled("b/w/i/d/a", key),
            Span::styled(" states  ", dim),
            Span::styled("f", key),
            Span::styled(" focus  ", dim),
            Span::styled("j/k/↑↓", key),
            Span::styled(" move  ", dim),
            Span::styled("esc", key),
            Span::styled(" close", dim),
        ])
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_end_uses_display_width() {
        let text = truncate_end("提交 herdr 的反馈", 16);

        assert_eq!(text, "提交 herdr 的反…");
        assert!(display_width(&text) <= 16);
    }

    #[test]
    fn middle_elide_uses_display_width() {
        let text = middle_elide("重构用户认证模块并迁移到统一登录服务", 12);

        assert!(text.contains('…'));
        assert!(display_width(&text) <= 12);
    }
}
