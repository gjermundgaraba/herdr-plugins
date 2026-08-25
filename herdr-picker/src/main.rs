mod config;
mod model;
mod process;
mod ui;

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    io::{self, IsTerminal},
    process::ExitCode,
    sync::mpsc::TryRecvError,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use config::{InputMode, SearchMode, Step, Workflow};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEventKind,
};
use model::{Item, Picker};
use ratatui::layout::{Position, Rect};
use serde_json::{Value, json};

const USAGE: &str = "\
Usage:
  herdr-picker run <name>
  herdr-picker check <name>
  herdr-picker list";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Direct,
    VimNormal,
    VimSearch,
}

impl From<InputMode> for Mode {
    fn from(mode: InputMode) -> Self {
        match mode {
            InputMode::Direct => Self::Direct,
            InputMode::Vim => Self::VimNormal,
        }
    }
}

fn main() -> ExitCode {
    if let Some(code) = process::maybe_run_submit_worker() {
        return code;
    }
    let pause_on_failure = process::running_in_popup();
    let cli = match parse_cli(env::args_os().skip(1)) {
        Ok(cli) => cli,
        Err(error) => return fail_visibly(&error, pause_on_failure),
    };
    match execute(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail_visibly(&error, pause_on_failure),
    }
}

fn execute(cli: Cli) -> Result<()> {
    match cli {
        Cli::Help => {
            print_help();
            Ok(())
        }
        Cli::Version => {
            println!("herdr-picker {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Cli::List => {
            let config_dir = config::config_dir()?;
            for name in config::list(&config_dir)? {
                println!("{name}");
            }
            Ok(())
        }
        Cli::Check { name } => {
            let config_dir = config::config_dir()?;
            Workflow::load(&name, &config_dir)?;
            println!("{name}: ok");
            Ok(())
        }
        Cli::Run { name } => {
            let config_dir = config::config_dir()?;
            let workflow = Workflow::load(&name, &config_dir)?;
            run_workflow(&name, &workflow)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run { name: String },
    Check { name: String },
    List,
    Help,
    Version,
}

fn parse_cli(args: impl IntoIterator<Item = OsString>) -> Result<Cli> {
    let args = args
        .into_iter()
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| anyhow::anyhow!("arguments must be valid UTF-8"))
        })
        .collect::<Result<Vec<_>>>()?;

    match args.as_slice() {
        [command, name] if command == "run" => Ok(Cli::Run { name: name.clone() }),
        [command, name] if command == "check" => Ok(Cli::Check { name: name.clone() }),
        [command] if command == "list" => Ok(Cli::List),
        [] => Ok(Cli::Help),
        [command] if matches!(command.as_str(), "-h" | "--help") => Ok(Cli::Help),
        [command] if matches!(command.as_str(), "-V" | "--version") => Ok(Cli::Version),
        _ => bail!(USAGE),
    }
}

fn print_help() {
    println!(
        "\
Declarative fuzzy picker for Herdr

{USAGE}

Picker directory:
  $HERDR_PICKER_CONFIG_DIR
  $XDG_CONFIG_HOME/herdr-picker/pickers
  $HOME/.config/herdr-picker/pickers"
    );
}

fn run_workflow(name: &str, workflow: &Workflow) -> Result<()> {
    let mut terminal = TerminalSession::new().context("cannot initialize terminal")?;
    let outcome = run_tui(&mut terminal.0, name, workflow);
    drop(terminal);

    if let Some(input) = outcome? {
        process::submit(&workflow.submit, &input)?;
    }
    Ok(())
}

struct TerminalSession(ratatui::DefaultTerminal);

impl TerminalSession {
    fn new() -> io::Result<Self> {
        let terminal = match ratatui::try_init() {
            Ok(terminal) => terminal,
            Err(error) => {
                ratatui::restore();
                return Err(error);
            }
        };
        if let Err(error) = crossterm::execute!(io::stdout(), EnableMouseCapture) {
            ratatui::restore();
            return Err(error);
        }
        Ok(Self(terminal))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}

struct ScreenState {
    step_index: usize,
    picker: Picker,
    mode: Mode,
}

struct StepRuntime {
    picker: Picker,
    mode: Mode,
    remote_search: bool,
    provider: Option<process::Provider>,
    loading: bool,
    error: Option<String>,
    received_snapshot: bool,
    pending_query: Option<Instant>,
    spinner_frame: usize,
    next_spinner: Instant,
}

impl StepRuntime {
    fn start(
        name: &str,
        step: &Step,
        selections: &BTreeMap<String, Item>,
        mode: Mode,
        restored: Option<Picker>,
    ) -> Self {
        let mut picker = restored.unwrap_or_else(|| {
            Picker::new(
                if step.source.is_some() {
                    Vec::new()
                } else {
                    step.items.clone()
                },
                step.search == SearchMode::Local,
            )
        });
        let (provider, error) = match &step.source {
            Some(source) => match process::Provider::start(
                source,
                command_input(name, step, selections, &picker.query),
            ) {
                Ok(provider) => (Some(provider), None),
                Err(error) => {
                    picker.clear_items();
                    (None, Some(format!("{error:#}")))
                }
            },
            None => (None, None),
        };
        let now = Instant::now();
        Self {
            picker,
            mode,
            remote_search: step.search == SearchMode::Provider,
            loading: provider.is_some(),
            provider,
            error,
            received_snapshot: false,
            pending_query: None,
            spinner_frame: 0,
            next_spinner: now + Duration::from_millis(100),
        }
    }

    fn into_state(self, step_index: usize) -> ScreenState {
        ScreenState {
            step_index,
            picker: self.picker,
            mode: self.mode,
        }
    }

    fn notice_query_change(&mut self, previous_query: &str) {
        if self.remote_search && self.picker.query != previous_query {
            self.provider.take();
            self.picker.clear_items();
            self.error = None;
            self.loading = true;
            self.received_snapshot = false;
            self.pending_query = Some(Instant::now() + Duration::from_millis(60));
        }
    }

    fn flush_pending(
        &mut self,
        now: Instant,
        name: &str,
        step: &Step,
        selections: &BTreeMap<String, Item>,
    ) -> bool {
        if self.pending_query.is_none_or(|deadline| deadline > now) {
            return false;
        }
        self.pending_query = None;
        let source = step.source.as_ref().unwrap();
        match process::Provider::start(
            source,
            command_input(name, step, selections, &self.picker.query),
        ) {
            Ok(provider) => self.provider = Some(provider),
            Err(error) => self.fail_provider(format!("{error:#}")),
        }
        true
    }

    fn poll_provider(&mut self) -> bool {
        let mut stop = false;
        let event = match self.provider.as_ref().map(process::Provider::try_recv) {
            Some(Ok(event)) => event,
            Some(Err(TryRecvError::Empty)) | None => return false,
            Some(Err(TryRecvError::Disconnected)) => {
                self.provider.take();
                let changed = self.loading;
                self.loading = false;
                return changed;
            }
        };
        match event {
            process::ProviderEvent::Snapshot(items) => {
                self.picker.replace_items(items);
                self.received_snapshot = true;
                self.loading = false;
                self.error = None;
            }
            process::ProviderEvent::Error(error) => {
                self.fail_provider(error);
                stop = true;
            }
            process::ProviderEvent::Exited { status, stderr } => {
                if !status.success() {
                    self.fail_provider(if stderr.is_empty() {
                        format!("provider exited with {status}")
                    } else {
                        format!("provider failed: {stderr}")
                    });
                } else if !self.received_snapshot {
                    self.fail_provider("provider exited before sending a snapshot".into());
                }
                self.loading = false;
                stop = true;
            }
        }
        if stop {
            self.provider.take();
            self.loading = false;
        }
        true
    }

    fn fail_provider(&mut self, error: String) {
        self.picker.clear_items();
        self.error = Some(error);
        self.loading = false;
    }

    fn tick_spinner(&mut self, now: Instant) -> bool {
        if !self.picker.needs_spinner() || now < self.next_spinner {
            return false;
        }
        self.spinner_frame = (self.spinner_frame + 1) % 10;
        self.next_spinner = now + Duration::from_millis(100);
        true
    }

    fn wait_duration(&self, now: Instant) -> Option<Duration> {
        let deadline = [
            self.pending_query,
            self.picker.needs_spinner().then_some(self.next_spinner),
        ]
        .into_iter()
        .flatten()
        .min();
        let timer = deadline.map(|deadline| deadline.saturating_duration_since(now));
        if self.provider.is_some() {
            // ponytail: poll the provider channel every 16ms; use OS-level multiplexing
            // only if profiling shows this popup-sized workload needs it.
            Some(
                timer
                    .unwrap_or(Duration::from_millis(16))
                    .min(Duration::from_millis(16)),
            )
        } else {
            timer
        }
    }
}

fn run_tui(
    terminal: &mut ratatui::DefaultTerminal,
    name: &str,
    workflow: &Workflow,
) -> Result<Option<Value>> {
    let mut selections = BTreeMap::new();
    let mut step_index = 0;
    let mut history: Vec<ScreenState> = Vec::new();
    let mut runtime = StepRuntime::start(
        name,
        &workflow.steps[0],
        &selections,
        workflow.mode.into(),
        None,
    );
    loop {
        let step = &workflow.steps[step_index];
        match run_screen(
            terminal,
            name,
            workflow,
            step_index,
            &selections,
            &mut runtime,
        )? {
            ScreenOutcome::Cancel => return Ok(None),
            ScreenOutcome::Back => {
                let Some(previous) = history.pop() else {
                    return Ok(None);
                };
                selections.remove(&workflow.steps[previous.step_index].id);
                step_index = previous.step_index;
                runtime = StepRuntime::start(
                    name,
                    &workflow.steps[step_index],
                    &selections,
                    previous.mode,
                    Some(previous.picker),
                );
            }
            ScreenOutcome::Select { item, query } => {
                selections.insert(step.id.clone(), *item);
                if step_index + 1 == workflow.steps.len() {
                    return Ok(Some(command_input(name, step, &selections, &query)));
                }

                let next_index = step_index + 1;
                history.push(runtime.into_state(step_index));
                step_index = next_index;
                runtime = StepRuntime::start(
                    name,
                    &workflow.steps[next_index],
                    &selections,
                    workflow.mode.into(),
                    None,
                );
            }
        }
    }
}

fn command_input(
    workflow: &str,
    step: &Step,
    selections: &BTreeMap<String, Item>,
    query: &str,
) -> Value {
    let selections = selections
        .iter()
        .map(|(step, item)| (step, json!({ "id": item.id, "value": item.value })))
        .collect::<BTreeMap<_, _>>();
    json!({
        "workflow": workflow,
        "step": step.id,
        "query": query,
        "selections": selections,
    })
}

enum ScreenOutcome {
    Select { item: Box<Item>, query: String },
    Back,
    Cancel,
}

fn run_screen(
    terminal: &mut ratatui::DefaultTerminal,
    name: &str,
    workflow: &Workflow,
    step_index: usize,
    selections: &BTreeMap<String, Item>,
    runtime: &mut StepRuntime,
) -> Result<ScreenOutcome> {
    let step = &workflow.steps[step_index];
    let mut dirty = true;
    loop {
        let now = Instant::now();
        dirty |= runtime.flush_pending(now, name, step, selections);
        dirty |= runtime.poll_provider();
        dirty |= runtime.tick_spinner(now);
        if dirty {
            let screen = ui::Screen {
                workflow_title: &workflow.title,
                step_title: &step.title,
                step_number: step_index + 1,
                step_count: workflow.steps.len(),
                error: runtime.error.as_deref(),
                loading: runtime.loading,
                spinner_frame: runtime.spinner_frame,
            };
            terminal.draw(|frame| ui::render(&mut runtime.picker, runtime.mode, &screen, frame))?;
            dirty = false;
        }

        let event = match runtime.wait_duration(Instant::now()) {
            Some(timeout) => {
                if event::poll(timeout)? {
                    Some(event::read()?)
                } else {
                    None
                }
            }
            None => Some(event::read()?),
        };
        let Some(event) = event else {
            continue;
        };
        let previous_query = runtime.picker.query.clone();
        match event {
            Event::Key(key) if key.is_press() => {
                if let Some(outcome) = handle_key(&mut runtime.picker, &mut runtime.mode, key) {
                    return Ok(outcome);
                }
            }
            Event::Mouse(mouse) => {
                let rects = ui::rects(terminal.size()?.into());
                match mouse.kind {
                    MouseEventKind::Moved => {
                        if let Some(index) =
                            row_at(&runtime.picker, rects.body, mouse.column, mouse.row)
                            && index != runtime.picker.selected
                        {
                            runtime.picker.selected = index;
                            runtime.picker.ensure_selection_visible();
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if ui::back_button_rect(rects.footer, step_index + 1)
                            .contains(Position::new(mouse.column, mouse.row))
                        {
                            return Ok(ScreenOutcome::Back);
                        } else if let Some(index) =
                            row_at(&runtime.picker, rects.body, mouse.column, mouse.row)
                        {
                            runtime.picker.selected = index;
                            if let Some(item) = runtime.picker.selected_item() {
                                return Ok(ScreenOutcome::Select {
                                    item: Box::new(item.clone()),
                                    query: runtime.picker.query.clone(),
                                });
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => runtime.picker.move_selection(3),
                    MouseEventKind::ScrollUp => runtime.picker.move_selection(-3),
                    _ => {}
                }
            }
            _ => {}
        }
        runtime.notice_query_change(&previous_query);
        dirty = true;
    }
}

fn handle_key(picker: &mut Picker, mode: &mut Mode, key: KeyEvent) -> Option<ScreenOutcome> {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) if *mode == Mode::VimSearch => {
            *mode = Mode::VimNormal;
            return None;
        }
        (KeyCode::Esc, _) => {
            return Some(ScreenOutcome::Back);
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Some(ScreenOutcome::Cancel),
        (KeyCode::Enter, _) => {
            return picker
                .selected_item()
                .cloned()
                .map(|item| ScreenOutcome::Select {
                    item: Box::new(item),
                    query: picker.query.clone(),
                });
        }
        _ => {}
    }

    match (key.code, key.modifiers) {
        (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
            picker.move_selection(-1)
        }
        (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
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
        (KeyCode::PageUp, KeyModifiers::NONE) => {
            picker.selected = picker.selected.saturating_sub(picker.visible_rows.max(1));
            picker.ensure_selection_visible();
        }
        (KeyCode::PageDown, KeyModifiers::NONE) => {
            picker.selected = picker
                .selected
                .saturating_add(picker.visible_rows.max(1))
                .min(picker.len().saturating_sub(1));
            picker.ensure_selection_visible();
        }
        (KeyCode::Home, KeyModifiers::NONE) => {
            picker.selected = 0;
            picker.ensure_selection_visible();
        }
        (KeyCode::End, KeyModifiers::NONE) => {
            picker.selected = picker.len().saturating_sub(1);
            picker.ensure_selection_visible();
        }
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
                && !modifiers.intersects(
                    KeyModifiers::CONTROL
                        | KeyModifiers::ALT
                        | KeyModifiers::SUPER
                        | KeyModifiers::META
                        | KeyModifiers::HYPER,
                ) =>
        {
            picker.query.push(character);
            picker.refilter();
        }
        _ => {}
    }
    None
}

fn row_at(picker: &Picker, body: Rect, column: u16, row: u16) -> Option<usize> {
    if !body.contains(Position::new(column, row))
        || row - body.y >= body.height / ui::ROW_HEIGHT * ui::ROW_HEIGHT
    {
        return None;
    }
    let index = picker.scroll + ((row - body.y) / ui::ROW_HEIGHT) as usize;
    (index < picker.len()).then_some(index)
}

fn fail_visibly(error: &anyhow::Error, pause: bool) -> ExitCode {
    eprintln!("herdr-picker: {error:#}");
    if pause && io::stdin().is_terminal() {
        eprintln!("Press Enter to close.");
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> Item {
        Item {
            id: id.into(),
            title: id.into(),
            ..Item::default()
        }
    }

    fn picker() -> Picker {
        Picker::new(vec![item("one"), item("two")], true)
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
    }

    #[test]
    fn ctrl_c_cancels_while_escape_navigates_back() {
        let mut picker = picker();
        let mut mode = Mode::Direct;

        assert!(matches!(
            handle_key(
                &mut picker,
                &mut mode,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            ),
            Some(ScreenOutcome::Cancel)
        ));
        assert!(matches!(
            handle_key(
                &mut picker,
                &mut mode,
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            ),
            Some(ScreenOutcome::Back)
        ));
    }

    #[test]
    fn command_context_contains_only_compact_selections() {
        let step = Step {
            id: "harness".into(),
            title: "Harness".into(),
            source: None,
            search: SearchMode::Local,
            items: vec![item("claude")],
        };
        let mut selections = BTreeMap::new();
        let mut selected = item("opus");
        selected.subtitle = "presentation".into();
        selections.insert("model".into(), selected);
        let input = command_input("new-agent", &step, &selections, "cl");

        assert_eq!(input["workflow"], "new-agent");
        assert_eq!(input["step"], "harness");
        assert_eq!(input["query"], "cl");
        assert_eq!(input["selections"]["model"]["id"], "opus");
        assert!(input["selections"]["model"].get("subtitle").is_none());
    }

    #[test]
    fn cli_supports_direct_invocation() {
        assert_eq!(
            parse_cli(["run", "sessions"].map(OsString::from)).unwrap(),
            Cli::Run {
                name: "sessions".into()
            },
        );
        assert_eq!(
            parse_cli(["run", "-sessions"].map(OsString::from)).unwrap(),
            Cli::Run {
                name: "-sessions".into()
            },
        );
        let usage = parse_cli(["check"].map(OsString::from))
            .unwrap_err()
            .to_string();
        assert!(usage.contains("check <name>"));
        assert!(usage.contains("list"));
    }

    #[test]
    fn page_navigation_clamps_at_the_ends() {
        let mut picker = picker();
        picker.visible_rows = 10;
        let mut mode = Mode::Direct;

        handle_key(
            &mut picker,
            &mut mode,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        );
        assert_eq!(picker.selected, 1);
        handle_key(
            &mut picker,
            &mut mode,
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        );
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn mouse_ignores_an_unpainted_partial_row() {
        let picker = picker();
        let body = Rect::new(0, 0, 20, 3);

        assert_eq!(row_at(&picker, body, 0, 1), Some(0));
        assert_eq!(row_at(&picker, body, 0, 2), None);
    }

    #[test]
    fn remote_query_change_clears_results_and_replaces_the_pending_query() {
        let now = Instant::now();
        let mut runtime = StepRuntime {
            picker: picker(),
            mode: Mode::Direct,
            remote_search: true,
            provider: None,
            loading: false,
            error: Some("old".into()),
            received_snapshot: true,
            pending_query: None,
            spinner_frame: 0,
            next_spinner: now,
        };

        runtime.picker.query = "n".into();
        runtime.notice_query_change("");
        let stale_deadline = Instant::now() + Duration::from_secs(60);
        runtime.pending_query = Some(stale_deadline);
        assert!(runtime.picker.is_empty());
        assert!(runtime.loading);
        assert!(runtime.error.is_none());

        runtime.picker.query = "new".into();
        runtime.notice_query_change("n");
        assert!(runtime.pending_query.unwrap() < stale_deadline);
        assert!(
            handle_key(
                &mut runtime.picker,
                &mut runtime.mode,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            )
            .is_none()
        );
    }

    #[test]
    fn provider_failure_invalidates_partial_results() {
        let step = Step {
            id: "source".into(),
            title: "Source".into(),
            source: Some(vec![
                "sh".into(),
                "-c".into(),
                concat!(
                    "read input; ",
                    "printf '{\"items\":[",
                    "{\"id\":\"partial\",\"title\":\"Partial\"}]}\\n'; ",
                    "echo crashed >&2; exit 7"
                )
                .into(),
            ]),
            search: SearchMode::Local,
            items: Vec::new(),
        };
        let mut runtime = StepRuntime::start("test", &step, &BTreeMap::new(), Mode::Direct, None);
        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.provider.is_some() && Instant::now() < deadline {
            runtime.poll_provider();
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(runtime.provider.is_none());
        assert!(runtime.picker.items.is_empty());
        assert!(
            runtime
                .error
                .as_deref()
                .is_some_and(|error| error.contains("crashed"))
        );
    }

    #[test]
    fn provider_poll_processes_one_snapshot_at_a_time() {
        let step = Step {
            id: "source".into(),
            title: "Source".into(),
            source: Some(vec![
                "sh".into(),
                "-c".into(),
                concat!(
                    "read input; ",
                    "printf '{\"items\":[{\"id\":\"one\",\"title\":\"One\"}]}\\n'; ",
                    "printf '{\"items\":[{\"id\":\"two\",\"title\":\"Two\"}]}\\n'; ",
                    "sleep 1"
                )
                .into(),
            ]),
            search: SearchMode::Local,
            items: Vec::new(),
        };
        let mut runtime = StepRuntime::start("test", &step, &BTreeMap::new(), Mode::Direct, None);
        let deadline = Instant::now() + Duration::from_secs(1);
        while !runtime.poll_provider() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(runtime.picker.selected_item().unwrap().id, "one");
    }

    #[test]
    fn clean_provider_exit_requires_a_snapshot() {
        let step = Step {
            id: "source".into(),
            title: "Source".into(),
            source: Some(vec!["sh".into(), "-c".into(), "cat >/dev/null".into()]),
            search: SearchMode::Local,
            items: Vec::new(),
        };
        let mut runtime = StepRuntime::start("test", &step, &BTreeMap::new(), Mode::Direct, None);
        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.provider.is_some() && Instant::now() < deadline {
            runtime.poll_provider();
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(
            runtime
                .error
                .as_deref()
                .is_some_and(|error| error.contains("before sending a snapshot"))
        );
    }

    #[test]
    fn source_startup_failure_stays_in_the_step() {
        let step = Step {
            id: "source".into(),
            title: "Source".into(),
            source: Some(vec!["definitely-not-a-picker-source".into()]),
            search: SearchMode::Local,
            items: Vec::new(),
        };

        let runtime = StepRuntime::start("test", &step, &BTreeMap::new(), Mode::Direct, None);

        assert!(runtime.provider.is_none());
        assert!(!runtime.loading);
        assert!(
            runtime
                .error
                .as_deref()
                .is_some_and(|error| error.contains("cannot start"))
        );
    }
}
