//! herdr-clanker-picker: an agent inbox for herdr, as a plugin popup.
//! Every agent in the session, attention-ranked; Enter jumps to it.
//!
//! Lifecycle adapted from yoshiori/herdr-configurable-picker (MIT-ish
//! reference for the proven popup mechanics); key and mouse handling copied
//! from herdr src/app/input/{modal,overlays}.rs (Apache-2.0).

mod config;
mod model;
mod ui;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

use herdr_client::Client;
use model::{Picker, StateFilter};

enum Outcome {
    Continue,
    Cancel,
    /// Jump to this pane and close.
    Accept(String),
}

fn main() -> ExitCode {
    let Some(socket_path) = std::env::var_os("HERDR_SOCKET_PATH") else {
        eprintln!(
            "herdr-clanker-picker must run inside a herdr session \
             (HERDR_SOCKET_PATH is not set).\n\
             Install the plugin and open it via its \"open\" action; \
             see README.md."
        );
        return ExitCode::from(2);
    };

    let config_dir = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR").map(PathBuf::from);
    let palette = config::resolve_palette(config_dir.as_deref());

    let client = Client::new(Path::new(&socket_path));
    let snapshot = match client.snapshot() {
        Ok(snapshot) => snapshot,
        Err(e) => return fail_visibly(&format!("{e:#}")),
    };
    let mut picker = Picker::open(snapshot, context_focused_pane_id(), load_pins());

    // Poll with a timeout instead of blocking on input: idle timeouts advance
    // the tick that animates the working-status spinner, exactly like the
    // built-in's. The built-in recomputes its rows from live state on every
    // frame; the closest a snapshot client gets is refreshing about once a
    // second.
    const SPINNER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(125);
    const REFRESH_EVERY_TICKS: u32 = 8;
    let mut tick: u32 = 0;

    let mut terminal = ratatui::init();
    // ratatui::init() does not enable mouse reporting; failure here just
    // means keyboard-only, not a broken picker. If the host has mouse
    // capture off, no reports arrive and nothing breaks.
    let _ = crossterm::execute!(std::io::stdout(), event::EnableMouseCapture);
    let restore = || {
        let _ = crossterm::execute!(std::io::stdout(), event::DisableMouseCapture);
        ratatui::restore();
    };

    let selection = loop {
        if let Err(e) = terminal.draw(|frame| ui::render(&mut picker, &palette, tick, frame)) {
            restore();
            return fail_visibly(&format!("failed to draw: {e}"));
        }
        match event::poll(SPINNER_INTERVAL) {
            Ok(false) => {
                tick = tick.wrapping_add(1);
                if tick.is_multiple_of(REFRESH_EVERY_TICKS) {
                    // A failed refresh (herdr restarting?) keeps the last
                    // good snapshot; the next interval retries anyway.
                    if let Ok(snapshot) = client.snapshot() {
                        picker.replace_snapshot(snapshot);
                    }
                }
                continue;
            }
            Ok(true) => {}
            Err(_) => break None,
        }
        match event::read() {
            Ok(Event::Key(key)) => match handle_key(&mut picker, key) {
                Outcome::Continue => {}
                Outcome::Cancel => break None,
                Outcome::Accept(target) => break Some(target),
            },
            Ok(Event::Mouse(mouse)) => {
                let area = terminal
                    .size()
                    .map(|size| Rect::new(0, 0, size.width, size.height))
                    .unwrap_or_default();
                match handle_mouse(&mut picker, area, mouse) {
                    Outcome::Continue => {}
                    Outcome::Cancel => break None,
                    Outcome::Accept(target) => break Some(target),
                }
            }
            // Resize just needs the next draw; other events are ignored.
            Ok(_) => {}
            Err(_) => break None,
        }
    };
    restore();

    if let Some(pane_id) = selection {
        if let Err(e) = client.call_value("pane.focus", &serde_json::json!({ "pane_id": pane_id }))
        {
            // The popup (and its stderr) vanishes the moment we return, so
            // the log file is the only place this error can survive.
            report_warnings(&[format!("focus failed for pane {pane_id}: {e:#}")]);
        }
    }
    // Exit 0 even on cancel: the popup closing is the normal outcome, and
    // herdr raises a toast for non-zero exits.
    ExitCode::SUCCESS
}

/// Copied from herdr's `handle_navigator_key` (src/app/input/modal.rs).
/// Deviations: Esc in command mode exits the process instead of leaving a
/// modal, the state-filter keys toggle a multi-select set instead of
/// replacing a single filter, and the workspace-toggle key is gone with the
/// tree.
fn handle_key(picker: &mut Picker, key: KeyEvent) -> Outcome {
    if picker.search_focused {
        match key.code {
            KeyCode::Esc => {
                picker.search_focused = false;
            }
            KeyCode::Enter => return accept(picker),
            KeyCode::Backspace => {
                picker.state_filters.clear();
                picker.query.pop();
                picker.select_first_match();
            }
            KeyCode::Up => picker.move_selection(-1),
            KeyCode::Down => picker.move_selection(1),
            KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                picker.move_selection(1)
            }
            KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
                picker.move_selection(-1)
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                picker.query.clear();
                picker.state_filters.clear();
                picker.clamp_selection();
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                picker.state_filters.clear();
                picker.query.push(c);
                picker.select_first_match();
            }
            _ => {}
        }
        return Outcome::Continue;
    }

    match key.code {
        KeyCode::Esc => return Outcome::Cancel,
        KeyCode::Enter => return accept(picker),
        KeyCode::Char('/') => {
            picker.state_filters.clear();
            picker.search_focused = true;
            picker.clamp_selection();
        }
        KeyCode::Backspace if !picker.state_filters.is_empty() => {
            picker.state_filters.clear();
            picker.clamp_selection();
        }
        KeyCode::Char('a') if key.modifiers.is_empty() => {
            picker.query.clear();
            picker.state_filters.clear();
            picker.clamp_selection();
        }
        KeyCode::Char('b') if key.modifiers.is_empty() => {
            picker.toggle_state_filter(StateFilter::Blocked);
        }
        KeyCode::Char('w') if key.modifiers.is_empty() => {
            picker.toggle_state_filter(StateFilter::Working);
        }
        KeyCode::Char('i') if key.modifiers.is_empty() => {
            picker.toggle_state_filter(StateFilter::Idle);
        }
        KeyCode::Char('d') if key.modifiers.is_empty() => {
            picker.toggle_state_filter(StateFilter::Done);
        }
        KeyCode::Char('f') if key.modifiers.is_empty() => {
            picker.toggle_pin();
            save_pins(&picker.pinned);
        }
        KeyCode::Char('j') | KeyCode::Down => picker.move_selection(1),
        KeyCode::Char('k') | KeyCode::Up => picker.move_selection(-1),
        KeyCode::Char('d') if key.modifiers == KeyModifiers::CONTROL => {
            picker.move_selection((picker.visible_rows / 2).max(1) as isize)
        }
        KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
            picker.move_selection(-((picker.visible_rows / 2).max(1) as isize))
        }
        KeyCode::Home => {
            picker.selected = 0;
            picker.ensure_selection_visible();
        }
        KeyCode::End | KeyCode::Char('G') => {
            picker.selected = picker.rows().len().saturating_sub(1);
            picker.ensure_selection_visible();
        }
        _ => {}
    }
    Outcome::Continue
}

/// Copied from herdr's navigator mouse handling (src/app/input/overlays.rs).
/// The "click outside the popup closes" arm is unreachable here — the popup
/// owns the whole terminal surface — and the workspace-caret click is gone
/// with the tree.
fn handle_mouse(picker: &mut Picker, area: Rect, mouse: event::MouseEvent) -> Outcome {
    let rects = ui::nav_rects(area);
    match mouse.kind {
        MouseEventKind::Moved => {
            if let Some(idx) = row_index_at(picker, rects.body, mouse.column, mouse.row) {
                picker.selected = idx;
                picker.ensure_selection_visible();
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if rect_contains(rects.search, mouse.column, mouse.row) {
                picker.search_focused = true;
            } else if let Some(idx) = row_index_at(picker, rects.body, mouse.column, mouse.row) {
                picker.selected = idx;
                return accept(picker);
            }
        }
        MouseEventKind::ScrollUp => {
            picker.scroll = picker.scroll.saturating_sub(3);
            picker.align_selection_to_scroll();
        }
        MouseEventKind::ScrollDown => {
            let max = picker.max_scroll((rects.body.height / ui::ROW_HEIGHT) as usize);
            picker.scroll = picker.scroll.saturating_add(3).min(max);
            picker.align_selection_to_scroll();
        }
        _ => {}
    }
    Outcome::Continue
}

fn accept(picker: &Picker) -> Outcome {
    match picker.rows().get(picker.selected) {
        Some(row) => Outcome::Accept(row.pane_id.clone()),
        None => Outcome::Continue,
    }
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

fn row_index_at(picker: &Picker, body: Rect, col: u16, row: u16) -> Option<usize> {
    if !rect_contains(body, col, row) {
        return None;
    }
    // Rows are two lines tall; either line hits the same row.
    let idx = picker
        .scroll
        .saturating_add((row.saturating_sub(body.y) / ui::ROW_HEIGHT) as usize);
    (idx < picker.rows().len()).then_some(idx)
}

/// The pane the user came from, out of HERDR_PLUGIN_CONTEXT_JSON. Popups get
/// no HERDR_PANE_ID (they are not panes), so this is how the ◆ current
/// marker finds its row. Best effort.
fn context_focused_pane_id() -> Option<String> {
    let context = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok()?;
    let context: serde_json::Value = serde_json::from_str(&context).ok()?;
    Some(context.get("focused_pane_id")?.as_str()?.to_string())
}

/// Focus marks (the `f` key) live in $HERDR_PLUGIN_STATE_DIR/focus, one
/// pane id per line, so they survive across picker opens.
fn pins_path() -> Option<PathBuf> {
    std::env::var_os("HERDR_PLUGIN_STATE_DIR").map(|dir| PathBuf::from(dir).join("focus"))
}

fn load_pins() -> std::collections::HashSet<String> {
    pins_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|raw| {
            raw.lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn save_pins(pinned: &std::collections::HashSet<String>) {
    let Some(path) = pins_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut ids: Vec<&str> = pinned.iter().map(String::as_str).collect();
    ids.sort_unstable();
    let _ = std::fs::write(path, ids.join("\n") + "\n");
}

/// Stderr flashes and vanishes with the popup, so warnings also go to
/// $HERDR_PLUGIN_STATE_DIR/picker.log where they can be read later.
fn report_warnings(warnings: &[String]) {
    if warnings.is_empty() {
        return;
    }
    for warning in warnings {
        eprintln!("herdr-clanker-picker: {warning}");
    }
    if let Some(state_dir) = std::env::var_os("HERDR_PLUGIN_STATE_DIR") {
        let path = PathBuf::from(state_dir).join("picker.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            for warning in warnings {
                let _ = writeln!(file, "{warning}");
            }
        }
    }
}

/// Startup failure inside the popup: the pane closes as soon as we exit, so
/// hold the message on screen briefly. Exit 0 to avoid a duplicate toast.
fn fail_visibly(message: &str) -> ExitCode {
    report_warnings(&[message.to_string()]);
    std::thread::sleep(std::time::Duration::from_secs(3));
    ExitCode::SUCCESS
}
