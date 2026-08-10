//! Small, shared Ratatui chrome for Herdr popup plugins.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::Widget,
};

pub mod theme {
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
}

#[derive(Debug, Clone, Copy)]
pub struct SearchLine<'a> {
    pub query: &'a str,
    pub placeholder: &'a str,
    pub focused: bool,
}

impl<'a> SearchLine<'a> {
    pub fn line(&self) -> Line<'a> {
        Line::from(vec![
            Span::styled("/ ", theme::accent()),
            if self.query.is_empty() {
                Span::styled(self.placeholder, theme::muted())
            } else {
                Span::raw(self.query)
            },
        ])
    }

    pub fn cursor_position(&self, area: Rect) -> Option<(u16, u16)> {
        if !self.focused || area.width == 0 || area.height == 0 {
            return None;
        }
        let offset = 2usize
            .saturating_add(Line::from(self.query).width())
            .min(usize::from(area.width.saturating_sub(1))) as u16;
        Some((area.x.saturating_add(offset), area.y))
    }
}

impl Widget for &SearchLine<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        self.line().render(area, buffer);
    }
}

pub fn key_hints<'a>(pairs: &[(&'a str, &'a str)]) -> Line<'a> {
    Line::from(
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
            .collect::<Vec<_>>(),
    )
}

#[derive(Debug, Clone, Copy)]
pub struct Separator;

impl Widget for Separator {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        Line::styled("─".repeat(area.width as usize), theme::muted()).render(area, buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_and_hints_share_popup_chrome() {
        let search = SearchLine {
            query: "agent",
            placeholder: "type to search",
            focused: true,
        };
        assert_eq!(search.line().to_string(), "/ agent");
        assert_eq!(
            search.cursor_position(Rect::new(4, 2, 20, 1)),
            Some((11, 2))
        );
        assert_eq!(
            key_hints(&[("enter", "open"), ("esc", "close")]).to_string(),
            "enter open  esc close"
        );
    }
}
