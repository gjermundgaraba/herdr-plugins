mod dispatch;
mod model;
mod sources;
mod ui;

use std::{io, process::ExitCode};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEventKind,
};
use model::{Filter, Picker};

fn main() -> ExitCode {
    if let Some(code) = dispatch::maybe_run_worker() {
        return code;
    }
    if std::env::var("HERDR_ENV").ok().as_deref() != Some("1") {
        return fail_visibly("command-palette must run inside Herdr");
    }

    let client = match herdr_client::Client::from_env() {
        Ok(client) => client,
        Err(error) => return fail_visibly(&error.to_string()),
    };
    let items = match sources::collect(&client) {
        Ok(items) => items,
        Err(error) => return fail_visibly(&error),
    };
    let mut picker = Picker::new(items, Filter::All);

    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(io::stdout(), EnableMouseCapture);
    let selection = run(&mut terminal, &mut picker);
    let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();

    match selection {
        Ok(Some(dispatch)) => match dispatch::schedule(&dispatch) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail_visibly(&error),
        },
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => fail_visibly(&error.to_string()),
    }
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    picker: &mut Picker,
) -> io::Result<Option<model::Dispatch>> {
    loop {
        terminal.draw(|frame| ui::render(picker, frame))?;
        match event::read()? {
            Event::Key(key) if key.is_press() => {
                if let Some(outcome) = handle_key(picker, key) {
                    return Ok(outcome);
                }
            }
            Event::Mouse(mouse) => {
                let rects = ui::rects(terminal.size()?.into());
                match mouse.kind {
                    MouseEventKind::Moved => {
                        if let Some(index) = row_at(picker, rects.body, mouse.column, mouse.row) {
                            picker.selected = index;
                            picker.ensure_selection_visible();
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(index) = row_at(picker, rects.body, mouse.column, mouse.row) {
                            picker.selected = index;
                            if let Some(item) = picker.selected_item() {
                                return Ok(Some(item.dispatch.clone()));
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => picker.move_selection(3),
                    MouseEventKind::ScrollUp => picker.move_selection(-3),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn handle_key(picker: &mut Picker, key: KeyEvent) -> Option<Option<model::Dispatch>> {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Some(None),
        (KeyCode::Enter, _) => {
            if let Some(item) = picker.selected_item() {
                return Some(Some(item.dispatch.clone()));
            }
        }
        (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => picker.move_selection(-1),
        (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
            picker.move_selection(1)
        }
        (KeyCode::PageUp, _) => picker.move_selection(-(picker.visible_rows.max(1) as isize)),
        (KeyCode::PageDown, _) => picker.move_selection(picker.visible_rows.max(1) as isize),
        (KeyCode::Home, _) => {
            picker.selected = 0;
            picker.ensure_selection_visible();
        }
        (KeyCode::End, _) => {
            picker.selected = picker.rows().len().saturating_sub(1);
            picker.ensure_selection_visible();
        }
        (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::BackTab, _) => picker.cycle_filter(-1),
        (KeyCode::Tab, _) => picker.cycle_filter(1),
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => picker.set_filter(Filter::Actions),
        (KeyCode::Char('w'), KeyModifiers::CONTROL) => picker.set_filter(Filter::Workspaces),
        (KeyCode::Char('t'), KeyModifiers::CONTROL) => picker.set_filter(Filter::Tabs),
        (KeyCode::Char('p'), modifiers) if modifiers.contains(KeyModifiers::ALT) => {
            picker.set_filter(Filter::Panes)
        }
        (KeyCode::Char('g'), KeyModifiers::CONTROL) => picker.set_filter(Filter::Agents),
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            picker.query.clear();
            picker.refilter();
        }
        (KeyCode::Backspace, _) => {
            picker.query.pop();
            picker.refilter();
        }
        (KeyCode::Char(character), modifiers)
            if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            picker.query.push(character);
            picker.refilter();
        }
        _ => {}
    }
    None
}

fn row_at(picker: &Picker, body: ratatui::layout::Rect, column: u16, row: u16) -> Option<usize> {
    if column < body.x
        || column >= body.x + body.width
        || row < body.y
        || row >= body.y + body.height
    {
        return None;
    }
    let index = picker.scroll + ((row - body.y) / ui::ROW_HEIGHT) as usize;
    (index < picker.rows().len()).then_some(index)
}

fn fail_visibly(message: &str) -> ExitCode {
    eprintln!("herdr-command-palette: {message}");
    eprintln!("Press Enter to close.");
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    ExitCode::FAILURE
}
