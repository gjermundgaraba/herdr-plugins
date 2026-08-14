mod dispatch;
mod model;
mod sources;
mod ui;

use std::{
    collections::HashMap,
    fs, io,
    process::ExitCode,
    time::{Duration, Instant},
};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEventKind,
};
use herdr_client::{Environment, PluginInvocation, PluginPaths};
use model::{Filter, Picker};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Direct,
    VimNormal,
    VimSearch,
}

const SPINNER_TICK: Duration = Duration::from_millis(100);
fn main() -> ExitCode {
    if let Some(code) = dispatch::maybe_run_worker() {
        return code;
    }
    if let Some(code) = maybe_open_action() {
        return code;
    }
    let client = match herdr_client::Client::from_env() {
        Ok(client) => client,
        Err(error) => return fail_visibly(&error.to_string()),
    };
    let items = match sources::load(&client) {
        Ok(items) => items,
        Err(error) => return fail_visibly(&error),
    };
    let filter = match initial_filter() {
        Ok(filter) => filter,
        Err(error) => return fail_visibly(&error),
    };
    let mut picker = Picker::new(items, filter);
    let mut mode = match initial_mode() {
        Ok(mode) => mode,
        Err(error) => return fail_visibly(&error),
    };

    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(io::stdout(), EnableMouseCapture);
    let selection = run(&mut terminal, &mut picker, &mut mode);
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

#[derive(Debug, Default, Deserialize)]
struct ActionConfigFile {
    #[serde(default)]
    actions: HashMap<String, ActionConfig>,
}

#[derive(Debug, Deserialize)]
struct ActionConfig {
    mode: Option<String>,
}

fn maybe_open_action() -> Option<ExitCode> {
    let environment = match Environment::load() {
        Ok(environment) => environment,
        Err(error) => return Some(fail_visibly(&error.to_string())),
    };
    let Some(PluginInvocation::Action(action_id)) = environment.invocation() else {
        return None;
    };
    let filter = match action_id {
        "open" => None,
        "agents" => Some("agents"),
        "workspaces" => Some("workspaces"),
        _ => return Some(fail_visibly(&format!("unknown action: {action_id}"))),
    };
    let plugin = match environment.require_plugin() {
        Ok(plugin) => plugin,
        Err(error) => return Some(fail_visibly(&error.to_string())),
    };
    let mode = match action_mode(&plugin, action_id) {
        Ok(mode) => mode,
        Err(error) => return Some(fail_visibly(&error)),
    };

    let params = open_params(&plugin.plugin_id, filter, mode);
    let result = herdr_client::Client::from_env()
        .and_then(|client| client.call_value("plugin.pane.open", &params));
    Some(match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => fail_visibly(&format!("failed to open picker: {error}")),
    })
}

fn open_params(plugin_id: &str, filter: Option<&str>, mode: Mode) -> serde_json::Value {
    let mut env = serde_json::Map::new();
    if let Some(filter) = filter {
        env.insert("HERDR_PICKER_FILTER".into(), filter.into());
    }
    env.insert(
        "HERDR_PICKER_MODE".into(),
        match mode {
            Mode::Direct => "direct".into(),
            Mode::VimNormal | Mode::VimSearch => "vim".into(),
        },
    );
    serde_json::json!({
        "plugin_id": plugin_id,
        "entrypoint": "picker",
        "env": env,
    })
}

fn action_mode(plugin: &PluginPaths, action_id: &str) -> Result<Mode, String> {
    let path = plugin.config_file("config.toml");
    match fs::read_to_string(&path) {
        Ok(config) => mode_from_config(&config, action_id)
            .map_err(|error| format!("invalid {}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Mode::Direct),
        Err(error) => Err(format!("failed to read {}: {error}", path.display())),
    }
}

fn mode_from_config(config: &str, action_id: &str) -> Result<Mode, String> {
    let config: ActionConfigFile = toml::from_str(config).map_err(|error| error.to_string())?;
    match config
        .actions
        .get(action_id)
        .and_then(|action| action.mode.as_deref())
    {
        Some("vim") => Ok(Mode::VimNormal),
        Some("direct") | None => Ok(Mode::Direct),
        Some(_) => Err("mode must be direct or vim".into()),
    }
}

fn initial_filter() -> Result<Filter, String> {
    match std::env::var("HERDR_PICKER_FILTER") {
        Ok(value) => value
            .parse()
            .map_err(|error| format!("invalid HERDR_PICKER_FILTER: {error}")),
        Err(std::env::VarError::NotPresent) => Ok(Filter::All),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("HERDR_PICKER_FILTER is not valid UTF-8".into())
        }
    }
}

fn initial_mode() -> Result<Mode, String> {
    match std::env::var("HERDR_PICKER_MODE") {
        Ok(value) if value == "vim" => Ok(Mode::VimNormal),
        Ok(value) if value == "direct" => Ok(Mode::Direct),
        Err(std::env::VarError::NotPresent) => Ok(Mode::Direct),
        _ => Err("invalid HERDR_PICKER_MODE: expected direct or vim".into()),
    }
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    picker: &mut Picker,
    mode: &mut Mode,
) -> io::Result<Option<model::Dispatch>> {
    let mut spinner_frame = 0;
    let mut next_spinner_frame = Instant::now() + SPINNER_TICK;
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| ui::render(picker, *mode, spinner_frame, frame))?;
            dirty = false;
        }

        let spinner_active = picker.needs_spinner();
        if !spinner_active
            || event::poll(next_spinner_frame.saturating_duration_since(Instant::now()))?
        {
            // Drain every queued event before redrawing so fast typing and
            // held-key repeat cost one refilter each but only one draw.
            loop {
                match event::read()? {
                    Event::Key(key) if key.is_press() => {
                        if let Some(outcome) = handle_key(picker, mode, key) {
                            return Ok(outcome);
                        }
                        dirty = true;
                    }
                    Event::Mouse(mouse) => {
                        let rects = ui::rects(terminal.size()?.into());
                        match mouse.kind {
                            MouseEventKind::Moved => {
                                if let Some(index) =
                                    row_at(picker, rects.body, mouse.column, mouse.row)
                                    && index != picker.selected
                                {
                                    picker.selected = index;
                                    picker.ensure_selection_visible();
                                    dirty = true;
                                }
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                if let Some(index) =
                                    row_at(picker, rects.body, mouse.column, mouse.row)
                                {
                                    picker.selected = index;
                                    if let Some(item) = picker.selected_item() {
                                        return Ok(Some(item.dispatch.clone()));
                                    }
                                }
                            }
                            MouseEventKind::ScrollDown => {
                                picker.move_selection(3);
                                dirty = true;
                            }
                            MouseEventKind::ScrollUp => {
                                picker.move_selection(-3);
                                dirty = true;
                            }
                            _ => {}
                        }
                    }
                    Event::Resize(..) => dirty = true,
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if spinner_active
            && advance_spinner_frame(Instant::now(), &mut spinner_frame, &mut next_spinner_frame)
        {
            dirty = true;
        }
    }
}

fn advance_spinner_frame(
    now: Instant,
    spinner_frame: &mut usize,
    next_spinner_frame: &mut Instant,
) -> bool {
    if now >= *next_spinner_frame {
        *spinner_frame = spinner_frame.wrapping_add(1);
        *next_spinner_frame = now + SPINNER_TICK;
        true
    } else {
        false
    }
}

fn handle_key(
    picker: &mut Picker,
    mode: &mut Mode,
    key: KeyEvent,
) -> Option<Option<model::Dispatch>> {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) if *mode == Mode::VimSearch => *mode = Mode::VimNormal,
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
        (KeyCode::Char('k'), KeyModifiers::NONE) if *mode == Mode::VimNormal => {
            picker.move_selection(-1)
        }
        (KeyCode::Char('j'), KeyModifiers::NONE) if *mode == Mode::VimNormal => {
            picker.move_selection(1)
        }
        (KeyCode::Char('/'), KeyModifiers::NONE) if *mode == Mode::VimNormal => {
            *mode = Mode::VimSearch
        }
        (KeyCode::PageUp, _) => picker.move_selection(-(picker.visible_rows.max(1) as isize)),
        (KeyCode::PageDown, _) => picker.move_selection(picker.visible_rows.max(1) as isize),
        (KeyCode::Home, _) => {
            picker.selected = 0;
            picker.ensure_selection_visible();
        }
        (KeyCode::End, _) => {
            picker.selected = picker.len().saturating_sub(1);
            picker.ensure_selection_visible();
        }
        (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::BackTab, _) => picker.cycle_filter(-1),
        (KeyCode::Tab, _) => picker.cycle_filter(1),
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
        (KeyCode::Backspace, _) if *mode != Mode::VimNormal => {
            picker.query.pop();
            picker.refilter();
        }
        (KeyCode::Char(character), modifiers)
            if *mode != Mode::VimNormal
                && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
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
        || row - body.y >= body.height / ui::ROW_HEIGHT * ui::ROW_HEIGHT
    {
        return None;
    }
    let index = picker.scroll + ((row - body.y) / ui::ROW_HEIGHT) as usize;
    (index < picker.len()).then_some(index)
}

fn fail_visibly(message: &str) -> ExitCode {
    eprintln!("herdr-picker: {message}");
    eprintln!("Press Enter to close.");
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Dispatch, Item, Kind};
    use serde_json::json;

    fn picker() -> Picker {
        Picker::new(
            ["one", "two"]
                .into_iter()
                .map(|title| Item {
                    kind: Kind::Workspace,
                    agent_status: None,
                    title: title.into(),
                    subtitle: String::new(),
                    detail: String::new(),
                    dispatch: Dispatch::new("test", json!({})),
                })
                .collect(),
            Filter::All,
        )
    }

    #[test]
    fn vim_mode_navigates_until_search_starts() {
        let mut picker = picker();
        let mut mode = Mode::VimNormal;

        handle_key(
            &mut picker,
            &mut mode,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        assert_eq!(picker.selected, 1);
        assert!(picker.query.is_empty());

        handle_key(
            &mut picker,
            &mut mode,
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
        );
        handle_key(
            &mut picker,
            &mut mode,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        assert_eq!(picker.query, "j");

        assert!(
            handle_key(
                &mut picker,
                &mut mode,
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            )
            .is_none()
        );
        assert_eq!(mode, Mode::VimNormal);
    }

    #[test]
    fn overdue_spinner_frame_advances_once() {
        let now = Instant::now();
        let mut spinner_frame = 0;
        let mut next_spinner_frame = now;

        assert!(advance_spinner_frame(
            now,
            &mut spinner_frame,
            &mut next_spinner_frame
        ));

        assert_eq!(spinner_frame, 1);
        assert!(next_spinner_frame > now);
        assert!(!advance_spinner_frame(
            now,
            &mut spinner_frame,
            &mut next_spinner_frame
        ));
    }

    #[test]
    fn mouse_ignores_an_unpainted_partial_row() {
        let picker = picker();
        let body = ratatui::layout::Rect::new(0, 0, 20, 3);

        assert_eq!(row_at(&picker, body, 0, 1), Some(0));
        assert_eq!(row_at(&picker, body, 0, 2), None);
    }

    #[test]
    fn open_params_pass_filter_and_mode_through_env() {
        assert_eq!(
            open_params("example.plugin", Some("agents"), Mode::VimNormal),
            json!({
                "plugin_id": "example.plugin",
                "entrypoint": "picker",
                "env": {
                    "HERDR_PICKER_FILTER": "agents",
                    "HERDR_PICKER_MODE": "vim",
                },
            })
        );
        assert_eq!(
            open_params("example.plugin", None, Mode::Direct)["env"],
            json!({ "HERDR_PICKER_MODE": "direct" })
        );
    }

    #[test]
    fn action_config_defaults_and_overrides_mode() {
        assert_eq!(mode_from_config("", "agents").unwrap(), Mode::Direct);
        assert_eq!(
            mode_from_config("[actions.agents]\nmode = \"vim\"", "agents").unwrap(),
            Mode::VimNormal
        );
        assert!(mode_from_config("[actions.agents]\nmode = \"insert\"", "agents").is_err());
    }
}
