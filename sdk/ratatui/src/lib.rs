//! Small, shared Ratatui chrome for Herdr popup plugins.

use ratatui_core::{
    layout::Rect,
    terminal::Frame,
    text::{Line, Span},
};

pub mod theme {
    use ratatui_core::style::{Color, Modifier, Style};

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

const SEARCH_PROMPT: &str = "/ ";
const SEARCH_PROMPT_WIDTH: u16 = 2;

#[derive(Debug, Clone, Copy)]
pub struct SearchLine<'a> {
    pub query: &'a str,
    pub placeholder: &'a str,
    pub focused: bool,
}

impl<'a> SearchLine<'a> {
    pub fn desired_width(&self) -> usize {
        let text = if self.query.is_empty() {
            self.placeholder
        } else {
            self.query
        };
        usize::from(SEARCH_PROMPT_WIDTH)
            .saturating_add(Span::raw(text).width())
            .saturating_add(usize::from(self.focused && !self.query.is_empty()))
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        let area = area.intersection(frame.area());
        if area.is_empty() {
            return;
        }

        let prompt_width = area.width.min(SEARCH_PROMPT_WIDTH);
        let prompt_area = Rect::new(area.x, area.y, prompt_width, 1);
        frame.render_widget(Line::styled(SEARCH_PROMPT, theme::accent()), prompt_area);

        let input_area = Rect::new(
            area.x.saturating_add(prompt_width),
            area.y,
            area.width.saturating_sub(prompt_width),
            1,
        );
        if input_area.is_empty() {
            if self.focused {
                frame.set_cursor_position((area.right().saturating_sub(1), area.y));
            }
            return;
        }

        if self.query.is_empty() {
            frame.render_widget(Line::styled(self.placeholder, theme::muted()), input_area);
            if self.focused {
                frame.set_cursor_position((input_area.x, input_area.y));
            }
            return;
        }

        let query_width = Span::raw(self.query).width();
        let query_area = Rect {
            width: input_area.width.saturating_sub(u16::from(self.focused)),
            ..input_area
        };
        if !query_area.is_empty() {
            let query = Line::raw(self.query);
            let query = if query_width > usize::from(query_area.width) {
                query.right_aligned()
            } else {
                query
            };
            frame.render_widget(query, query_area);
        }

        if self.focused {
            let cursor_x = if query_width > usize::from(query_area.width) {
                input_area.right().saturating_sub(1)
            } else {
                query_area.x.saturating_add(
                    u16::try_from(query_width)
                        .unwrap_or(u16::MAX)
                        .min(query_area.width),
                )
            };
            frame.set_cursor_position((cursor_x, input_area.y));
        }
    }
}

pub fn key_hints<'a>(pairs: &[(&'a str, &'a str)]) -> Line<'a> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_core::{backend::TestBackend, layout::Position, terminal::Terminal};

    fn render_search(search: SearchLine<'_>, width: u16) -> (Vec<String>, Position) {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|frame| search.render(frame, frame.area()))
            .unwrap();
        let cells = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_owned())
            .collect();
        (cells, terminal.backend().cursor_position())
    }

    #[test]
    fn search_and_hints_share_popup_chrome() {
        let search = SearchLine {
            query: "agent",
            placeholder: "type to search",
            focused: true,
        };
        let (cells, cursor) = render_search(search, 8);
        assert_eq!(cells, ["/", " ", "a", "g", "e", "n", "t", " "]);
        assert_eq!(cursor, Position::new(7, 0));
        assert_eq!(search.desired_width(), 8);
        assert_eq!(
            key_hints(&[("enter", "open"), ("esc", "close")]).to_string(),
            "enter open  esc close"
        );
    }

    #[test]
    fn overflowing_search_keeps_the_suffix_visible_before_the_cursor() {
        let search = SearchLine {
            query: "abcdefghij",
            placeholder: "",
            focused: true,
        };
        let (cells, cursor) = render_search(search, 8);

        assert_eq!(cells, ["/", " ", "f", "g", "h", "i", "j", " "]);
        assert_eq!(cursor, Position::new(7, 0));
    }

    #[test]
    fn wide_search_never_places_the_cursor_on_a_continuation_cell() {
        let search = SearchLine {
            query: "界界",
            placeholder: "",
            focused: true,
        };
        let (cells, cursor) = render_search(search, 5);

        assert_eq!(cells, ["/", " ", "界", " ", " "]);
        assert_eq!(cursor, Position::new(4, 0));
        assert_eq!(cells[usize::from(cursor.x)], " ");
    }

    #[test]
    fn narrow_search_stays_in_bounds() {
        let search = SearchLine {
            query: "query",
            placeholder: "",
            focused: true,
        };
        let (cells, cursor) = render_search(search, 1);

        assert_eq!(cells, ["/"]);
        assert_eq!(cursor, Position::ORIGIN);
    }
}
