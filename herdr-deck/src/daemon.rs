use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use elgato_streamdeck::info::Kind;
use herdr_client::{
    ClientReportAgent,
    follow::{self, ClientKey, DaemonState, Directory, JumpTarget},
    sessions,
    slots::{SlotKey, resize_slots, sticky_slots},
};
use herdr_device::{
    Focus, JumpDispatcher, JumpError, Keystroke, OsInput, PostError, Refusal, SUBMIT_SETTLE, macos,
    selection_change, type_inputs,
};
use serde_json::json;
use signal_hook::consts::{SIGINT, SIGTERM};

use crate::clients::{DisplayModel, SessionState, active_endpoint, project_model};
use crate::config::{
    Action, AgentStatus, Config, CycleDirection, DeviceConfig, DeviceIdentity, DeviceRole,
    config_path, escape_xml, load_config, select_device_config,
};
use crate::device::{self, DeviceEvent, DeviceHandle, DeviceInfo, FrameBatch, InputEvent};
use crate::render::Renderer;
use crate::slots::agent_label;

const ANIMATION_TIMER_FPS: u64 = 30;
const ANIMATION_INTERVAL: Duration = Duration::from_micros(1_000_000 / ANIMATION_TIMER_FPS);
const WORKING_UPDATE_FPS: u64 = 15;
const DONE_UPDATE_FPS: u64 = 10;
const IDLE_INTERVAL_MS: u64 = 200;
const CONFIG_INTERVAL: Duration = Duration::from_secs(1);
const SCAN_INTERVAL: Duration = Duration::from_secs(2);
/// Keyboard work waiting for the action worker. A control that fires faster
/// than its prompts are typed is ignored beyond this, not queued.
const ACTION_QUEUE_CAPACITY: usize = 16;

pub fn main(args: Vec<String>) -> Result<(), String> {
    let command = args.first().map(String::as_str).unwrap_or("help");
    match command {
        "run" => {
            let path = config_path(args.get(1).map(String::as_str))?;
            println!("herdr-deck using {}", path.display());
            Daemon::new(path)?.run()
        }
        "devices" => {
            let devices = device::list_devices()?;
            println!("{}", devices_json(&devices)?);
            if devices.is_empty() {
                return Err("No devices visible. Quit Elgato Stream Deck, then retry.".into());
            }
            Ok(())
        }
        "check" => {
            let path = config_path(args.get(1).map(String::as_str))?;
            let config = load_config(&path)?;
            println!(
                "{}: valid ({} device rule{})",
                path.display(),
                config.devices.len(),
                if config.devices.len() == 1 { "" } else { "s" }
            );
            Ok(())
        }
        "doctor" => doctor(args.get(1).map(String::as_str)),
        "frames" => frames(
            args.get(1)
                .map(String::as_str)
                .ok_or("frames needs a state")?,
            args.get(2)
                .map(String::as_str)
                .ok_or("frames needs a directory")?,
            args.get(3).map(String::as_str),
        ),
        "push" => push(
            args.get(1).map(String::as_str).unwrap_or("herdr-deck.json"),
            args.get(2).map(String::as_str),
        ),
        "install-service" => install_service(
            args.get(1).map(String::as_str),
            args.get(2).map(String::as_str),
        ),
        "uninstall-service" => uninstall_service(args.get(1).map(String::as_str)),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => Err(format!("unknown command: {command}")),
    }
}

fn print_help() {
    println!(
        "Usage: herdr-deck <command>\n\n  devices                   List every visible Elgato device (Plus and Pedal are managed)\n  check [config]            Validate configuration\n  doctor [config]           Check config, Herdr clients, and device visibility\n  frames <state> <dir> [config]  Write one animation loop as JPEG key frames\n  push [source] [target]    Atomically install configuration\n  run [config]              Run the direct-HID daemon\n  install-service [plist] [config]  Install and start the launchd user service\n  uninstall-service [plist] Stop and remove the launchd user service\n"
    );
}

/// Off-device preview: render one full loop of a state's key at Stream Deck
/// Plus key size into `dir` as `<state>-<frame>.jpg`, using the configured
/// palette. Frame `n` is the image the device shows at `n / fps` seconds.
fn frames(state: &str, dir: &str, config_override: Option<&str>) -> Result<(), String> {
    let state = AgentStatus::ALL
        .into_iter()
        .find(|candidate| candidate.as_str() == state)
        .ok_or_else(|| {
            let names: Vec<_> = AgentStatus::ALL.iter().map(|s| s.as_str()).collect();
            format!("unknown state {state}; use one of {}", names.join(", "))
        })?;
    let config = load_config(config_path(config_override)?)?;
    let dir = PathBuf::from(dir);
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let agent: ClientReportAgent = serde_json::from_value(json!({
        "endpoint_id": "local", "focused": false, "workspace_label": "preview",
        "workspace_number": 1, "cwd": null, "foreground_cwd": null,
        "agent": {
            "pane_id": "preview", "workspace_id": "preview", "tab_id": "preview",
            "name": null, "display_agent": null, "agent": "claude", "title": "preview",
            "terminal_title": null, "terminal_title_stripped": null,
            "agent_status": state.as_str(), "state_change_seq": 0,
            "state_labels": [], "tokens": [], "focused": false,
        },
    }))
    .map_err(|error| error.to_string())?;
    let mut renderer = Renderer::new().map_err(|error| error.to_string())?;
    let crate::render::Loop {
        frames: count, fps, ..
    } = crate::render::animation(state);
    for frame in 0..count {
        let image = renderer
            .render_key(
                120,
                120,
                Some(&agent),
                false,
                false,
                None,
                &config.states,
                false,
                f64::from(frame) / f64::from(fps),
            )
            .map_err(|error| error.to_string())?;
        fs::write(
            dir.join(format!("{}-{frame:03}.jpg", state.as_str())),
            &image,
        )
        .map_err(|error| error.to_string())?;
    }
    println!(
        "wrote {count} frame{} at {fps} fps to {}",
        if count == 1 { "" } else { "s" },
        dir.display()
    );
    Ok(())
}

fn devices_json(devices: &[DeviceInfo]) -> Result<String, String> {
    let values = devices
        .iter()
        .map(|device| {
            json!({
                "model": device.model,
                "path": device.path,
                "serialNumber": device.serial_number,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&values).map_err(|error| error.to_string())
}

fn doctor(override_path: Option<&str>) -> Result<(), String> {
    let path = config_path(override_path)?;
    let config = load_config(&path)?;
    let sessions = sessions::discover().map_err(|error| error.to_string())?;
    let directory = follow::list_once(&sessions, Duration::from_secs(2), |session, error| {
        eprintln!("Herdr session {}: {error}", session.name)
    });
    if !directory
        .daemons
        .iter()
        .any(|daemon| matches!(daemon.state, DaemonState::Following { .. }))
    {
        return Err("no Herdr daemon answered client.list".into());
    }
    // One reading has no history: the running service follows the client
    // that last took focus, which this reading cannot name.
    println!(
        "clients: {} attached across {} session(s); {}",
        directory.client_count(),
        directory.daemons.len(),
        match directory.followed_client() {
            Some(client) if client.has_focus() => format!(
                "client {} of session {} has focus",
                client.client_id, client.session
            ),
            Some(client) => format!(
                "none has focus now; the dashboard follows the client that last took it, \
                 or client {} of session {} if none did since it started",
                client.client_id, client.session
            ),
            None => "none has sent a report, so the dashboard has no client to follow".into(),
        }
    );
    let connected = project_model(&directory).map_or(0, |model| {
        model
            .sessions
            .iter()
            .filter(|session| {
                session.connected
                    && (config.herdr.sessions.is_empty()
                        || config.herdr.sessions.contains(&session.key))
            })
            .count()
    });
    let devices = device::list_devices()?;
    let matched = devices
        .iter()
        .filter(|device| {
            supported(device.kind)
                && select_device_config(&config.devices, &identity(device)).is_some()
        })
        .count();
    println!("config: ok ({})", path.display());
    report_keyboard_access(&config);
    println!("endpoints: {connected} shown from the client a dashboard started now would follow");
    println!("devices: {matched}/{} managed and visible", devices.len());
    if matched == 0 {
        return Err("no configured devices visible; quit Elgato Stream Deck and retry".into());
    }
    Ok(())
}

fn push(source: &str, target: Option<&str>) -> Result<(), String> {
    let source = std::path::absolute(source).map_err(|error| error.to_string())?;
    let destination = config_path(target)?;
    load_config(&source)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = PathBuf::from(format!(
        "{}.{}.new",
        destination.to_string_lossy(),
        std::process::id()
    ));
    fs::copy(&source, &temporary).map_err(|error| error.to_string())?;
    fs::rename(&temporary, &destination).map_err(|error| error.to_string())?;
    println!("pushed {} -> {}", source.display(), destination.display());
    Ok(())
}

fn default_plist() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents/dev.herdr.deck.plist"))
}

fn service_path(override_path: Option<&str>) -> Result<PathBuf, String> {
    match override_path {
        Some(path) => std::path::absolute(path).map_err(|error| error.to_string()),
        None => default_plist(),
    }
}

fn install_service(plist_path: Option<&str>, config_override: Option<&str>) -> Result<(), String> {
    let destination = service_path(plist_path)?;
    let source = std::env::current_exe().map_err(|error| error.to_string())?;
    let config = config_path(config_override)?;
    load_config(&config)?;
    // launchd gives the service neither HERDR_BIN_PATH nor the user's PATH,
    // so the herdr it lists sessions with is fixed here, before anything is
    // installed.
    let herdr = sessions::resolve_herdr()
        .map_err(|error| format!("cannot find the herdr executable the service needs: {error}"))?;
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    let home = PathBuf::from(home);
    let working_directory = home.join("Library/Application Support/dev.herdr.deck");
    let executable = working_directory.join("bin/herdr-deck");
    let logs = home.join("Library/Logs/dev.herdr.deck");
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(&logs).map_err(|error| error.to_string())?;
    atomic_install(&source, &executable)?;
    let log = logs.join("daemon.log");
    let plist = service_plist(&executable, &config, &working_directory, &log, &herdr);
    fs::write(&destination, plist).map_err(|error| error.to_string())?;
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let path = destination.to_string_lossy().into_owned();
    run_launchctl(&["bootout", &domain, &path], true)?;
    run_launchctl(&["bootstrap", &domain, &path], false)?;
    println!(
        "installed {} and started {}",
        executable.display(),
        destination.display()
    );
    Ok(())
}

fn service_plist(
    executable: &Path,
    config: &Path,
    working_directory: &Path,
    log: &Path,
    herdr: &Path,
) -> String {
    let [executable, config, working_directory, log, herdr] =
        [executable, config, working_directory, log, herdr]
            .map(|path| escape_xml(&path.to_string_lossy()));
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>dev.herdr.deck</string>\n<key>ProgramArguments</key><array><string>{executable}</string><string>run</string><string>{config}</string></array>\n<key>WorkingDirectory</key><string>{working_directory}</string>\n<key>EnvironmentVariables</key><dict><key>HERDR_BIN_PATH</key><string>{herdr}</string></dict>\n<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>\n<key>ThrottleInterval</key><integer>5</integer>\n<key>StandardOutPath</key><string>{log}</string>\n<key>StandardErrorPath</key><string>{log}</string>\n</dict></plist>\n"
    )
}

// Replace the directory entry, never truncate an executable a daemon may be using.
// This also permits reinstalling from the installed executable itself.
fn atomic_install(source: &Path, destination: &Path) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or("executable has no parent directory")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(".herdr-deck.{}.new", std::process::id()));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    let result = (|| -> std::io::Result<()> {
        let mut input = fs::File::open(source)?;
        std::io::copy(&mut input, &mut output)?;
        output.set_permissions(fs::Permissions::from_mode(0o755))?;
        output.sync_all()?;
        fs::rename(&temporary, destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| error.to_string())
}

fn uninstall_service(plist_path: Option<&str>) -> Result<(), String> {
    let destination = service_path(plist_path)?;
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let path = destination.to_string_lossy().into_owned();
    run_launchctl(&["bootout", &domain, &path], true)?;
    match fs::remove_file(&destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    println!("stopped and removed {}", destination.display());
    Ok(())
}

fn run_launchctl(args: &[&str], ignore_failure: bool) -> Result<(), String> {
    let mut child = Command::new("/bin/launchctl")
        .args(args)
        .spawn()
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() || ignore_failure => return Ok(()),
            Ok(Some(status)) => return Err(format!("launchctl failed: {status}")),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("launchctl timed out".into());
            }
            Err(_) if ignore_failure => return Ok(()),
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ButtonSignature {
    offline: bool,
    pane_id: Option<String>,
    status: Option<String>,
    focused: bool,
    active_session: bool,
    title: Option<String>,
    subtitle: Option<String>,
    session: String,
    session_badge: Option<String>,
    tick: u64,
    config_version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TouchSignature {
    offline: bool,
    session: String,
    working: usize,
    done: usize,
    blocked: usize,
    config_version: u64,
}

struct ManagedDevice {
    info: DeviceInfo,
    config: DeviceConfig,
    handle: DeviceHandle,
    button_signatures: HashMap<u8, ButtonSignature>,
    touch_signature: Option<TouchSignature>,
}

enum Event {
    Animation,
    AnimationFailed(String),
    Scan(Result<Vec<DeviceInfo>, String>),
    Clients(Directory),
    Device(DeviceEvent),
    Action {
        name: &'static str,
        result: Result<(), String>,
    },
}

enum ActionCommand {
    SystemKey(crate::system_input::Key),
    /// A prompt or submit: keyboard events for the frontmost app. Before each
    /// keystroke, `client` must still be the followed Herdr client, with its
    /// window focused, in the last report the follower published.
    Input {
        name: &'static str,
        client: ClientKey,
        inputs: Vec<OsInput>,
    },
}

/// What a Herdr-facing action does, captured when its control is pressed.
#[derive(Debug, PartialEq, Eq)]
enum Planned {
    /// Ask the followed client to show the slot's agent.
    Jump(JumpTarget),
    Input {
        client: ClientKey,
        inputs: Vec<OsInput>,
    },
}

/// Captures a `focus-slot`, `prompt`, or `submit` against the followed
/// client. Without one, nothing is dispatched. A prompt or submit is typed
/// into the frontmost app, so it also needs that client's window focused,
/// here and again before every keystroke.
fn plan_action(
    action: &Action,
    directory: &Directory,
    slot: Option<&ClientReportAgent>,
) -> Result<Planned, String> {
    let client = directory
        .followed_client()
        .ok_or("no followed Herdr client")?;
    let input = |inputs| {
        if !client.has_focus() {
            return Err(Refusal::NotFocused.to_string());
        }
        Ok(Planned::Input {
            client: client.key(),
            inputs,
        })
    };
    match action {
        Action::FocusSlot { .. } => {
            let row = slot.ok_or("agent slot is empty")?;
            client
                .jump_to(row)
                .map(Planned::Jump)
                .ok_or_else(|| "the agent's endpoint is not online".into())
        }
        Action::Prompt { text, submit } => input(OsInput::prompt(text, submit.unwrap_or(true))),
        Action::Submit => input(vec![OsInput::Enter]),
        Action::SystemEnter | Action::SystemKey { .. } | Action::CycleSession { .. } => {
            Err("not a Herdr action".into())
        }
    }
}

struct Workers {
    scan_stop: mpsc::Sender<()>,
    /// The action worker's queue. The worker ends when this is dropped.
    actions: SyncSender<ActionCommand>,
    /// The followed client and its window focus, as the follower last saw
    /// them.
    focus: Focus,
    threads: Vec<JoinHandle<()>>,
}

impl Workers {
    fn stop(self) {
        // First, so that queued prompts, submits, and keys are dropped
        // rather than typed into whatever app is in front by now.
        self.focus.stop();
        let _ = self.scan_stop.send(());
        drop(self.actions);
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionFilter {
    All,
    Active,
    Session(String),
}

struct Daemon {
    path: PathBuf,
    config: Config,
    config_modified: Option<SystemTime>,
    config_version: u64,
    filter: SessionFilter,
    /// Every followed Herdr daemon and its attached clients.
    directory: Directory,
    model: Option<DisplayModel>,
    slots: Vec<Option<SlotKey>>,
    /// The client `slots` belong to. They survive a gap in which there is
    /// no followed client to show, so its keys mean the same afterwards.
    slots_client: Option<ClientKey>,
    devices: HashMap<String, ManagedDevice>,
    visible: Vec<DeviceInfo>,
    renderer: Renderer,
    events: Receiver<Event>,
    event_sender: mpsc::Sender<Event>,
    workers: Option<Workers>,
    /// Sends `focus-slot` jumps in the order this loop received the presses.
    jumps: JumpDispatcher,
    stopped: Arc<AtomicBool>,
    render_dirty: bool,
    failure: Option<String>,
}

impl Daemon {
    fn new(path: PathBuf) -> Result<Self, String> {
        let config = load_config(&path)?;
        let renderer = Renderer::new().map_err(|error| error.to_string())?;
        let stopped = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(SIGINT, Arc::clone(&stopped))
            .and_then(|_| signal_hook::flag::register(SIGTERM, Arc::clone(&stopped)))
            .map_err(|error| error.to_string())?;
        let (event_sender, events) = mpsc::channel();
        let workers = spawn_workers(event_sender.clone(), Arc::clone(&stopped));
        Ok(Self::with_workers(
            path,
            config,
            renderer,
            workers,
            (event_sender, events),
            stopped,
        ))
    }

    /// The daemon around its workers, which send their events to `events`.
    fn with_workers(
        path: PathBuf,
        config: Config,
        renderer: Renderer,
        workers: Workers,
        (event_sender, events): (mpsc::Sender<Event>, Receiver<Event>),
        stopped: Arc<AtomicBool>,
    ) -> Self {
        let config_modified = fs::metadata(&path).and_then(|value| value.modified()).ok();
        let jumps = JumpDispatcher::new(workers.focus.clone());
        Self {
            path,
            filter: SessionFilter::All,
            slots: vec![None; config.herdr.slot_count],
            config,
            config_modified,
            config_version: 0,
            directory: Directory::default(),
            model: None,
            slots_client: None,
            devices: HashMap::new(),
            visible: Vec::new(),
            renderer,
            events,
            event_sender,
            workers: Some(workers),
            jumps,
            stopped,
            render_dirty: true,
            failure: None,
        }
    }

    fn run(mut self) -> Result<(), String> {
        report_keyboard_access(&self.config);
        let mut next_config = Instant::now() + CONFIG_INTERVAL;
        while !self.stopped.load(Ordering::Acquire) {
            let now = Instant::now();
            if now >= next_config {
                while next_config <= now {
                    next_config += CONFIG_INTERVAL;
                }
                self.poll_config();
            }
            if self.render_dirty {
                self.render_dirty = false;
                self.render();
            }
            let timeout = next_config.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(timeout) {
                Ok(event) => {
                    self.handle_event(event);
                    while let Ok(event) = self.events.try_recv() {
                        self.handle_event(event);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        let finished = Arc::new(AtomicBool::new(false));
        let watchdog = Arc::clone(&finished);
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(3));
            if !watchdog.load(Ordering::Acquire) {
                unsafe { libc::_exit(0) };
            }
        });
        self.stopped.store(true, Ordering::Release);
        self.devices.clear();
        if let Some(workers) = self.workers.take() {
            workers.stop();
        }
        finished.store(true, Ordering::Release);
        match self.failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Animation => {
                if self.shown_agents().iter().any(|(session, row)| {
                    session.connected
                        && matches!(row.agent.agent_status.as_str(), "working" | "done" | "idle")
                }) {
                    self.render_dirty = true;
                }
            }
            Event::AnimationFailed(error) => {
                self.failure = Some(format!("animation timer failed: {error}"));
                self.stopped.store(true, Ordering::Release);
            }
            Event::Scan(Ok(devices)) => self.reconcile(devices),
            Event::Scan(Err(error)) => eprintln!("device scan failed: {error}"),
            Event::Clients(directory) => {
                let model = project_model(&directory);
                if self.model.is_some() && model.is_none() {
                    eprintln!("Herdr clients unavailable");
                }
                if let Some(line) = selection_change(&self.directory, &directory) {
                    println!("{line}");
                }
                self.directory = directory;
                self.model = model;
                self.normalize_filter();
                self.reassign_slots();
                self.render_dirty = true;
            }
            Event::Device(event) => self.handle_device_event(event),
            Event::Action { name, result } => match result {
                Ok(()) => {}
                Err(error) => eprintln!("{name} failed: {error}"),
            },
        }
    }

    fn handle_device_event(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::Ready(info) => {
                if self.devices.contains_key(&info.serial_number) {
                    println!("claimed {} {}", info.model, info.serial_number);
                    self.render_dirty = true;
                }
            }
            DeviceEvent::Input {
                serial_number,
                event,
            } => self.handle_input(&serial_number, event),
            DeviceEvent::Failed {
                serial_number,
                error,
            } => {
                if let Some(device) = self.devices.remove(&serial_number) {
                    eprintln!(
                        "released {} {}: {error}",
                        device.info.model, device.info.serial_number
                    );
                }
            }
        }
    }

    fn reconcile(&mut self, visible: Vec<DeviceInfo>) {
        let found = visible
            .iter()
            .map(|device| device.serial_number.as_str())
            .collect::<std::collections::HashSet<_>>();
        let missing = self
            .devices
            .keys()
            .filter(|serial| !found.contains(serial.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for serial in missing {
            if let Some(device) = self.devices.remove(&serial) {
                eprintln!("released {} {}: disconnected", device.info.model, serial);
            }
        }
        self.visible = visible;
        for info in self.visible.clone() {
            if self.devices.contains_key(&info.serial_number) || !supported(info.kind) {
                continue;
            }
            let Some(config) =
                select_device_config(&self.config.devices, &identity(&info)).cloned()
            else {
                continue;
            };
            let tx = self.event_sender.clone();
            let handle = DeviceHandle::spawn(info.clone(), config.brightness, move |event| {
                let _ = tx.send(Event::Device(event));
            });
            self.devices.insert(
                info.serial_number.clone(),
                ManagedDevice {
                    info,
                    config,
                    handle,
                    button_signatures: HashMap::new(),
                    touch_signature: None,
                },
            );
        }
    }

    fn poll_config(&mut self) {
        let modified = match fs::metadata(&self.path).and_then(|value| value.modified()) {
            Ok(modified) => modified,
            Err(_) => return,
        };
        if self.config_modified == Some(modified) {
            return;
        }
        self.config_modified = Some(modified);
        match load_config(&self.path) {
            Ok(next) => self.reload(next),
            Err(error) => eprintln!("config reload rejected: {error}"),
        }
    }

    fn reload(&mut self, next: Config) {
        self.config = next;
        self.config_version += 1;
        self.normalize_filter();
        self.slots = resize_slots(&self.slots, self.config.herdr.slot_count);
        self.reassign_slots();

        let mut disabled = Vec::new();
        for (serial, device) in &mut self.devices {
            match select_device_config(&self.config.devices, &identity(&device.info)).cloned() {
                Some(config) => {
                    device.config = config;
                    device.button_signatures.clear();
                    device.touch_signature = None;
                    device.handle.set_brightness(device.config.brightness);
                }
                None => disabled.push(serial.clone()),
            }
        }
        for serial in disabled {
            if let Some(device) = self.devices.remove(&serial) {
                eprintln!(
                    "released {} {}: disabled by configuration",
                    device.info.model, serial
                );
            }
        }
        self.reconcile(self.visible.clone());
        println!("reloaded {}", self.path.display());
        self.render_dirty = true;
    }

    fn cycle_filter(&mut self, direction: CycleDirection) {
        let sessions = self.connected_session_keys();
        self.filter = cycle_filter(&self.filter, &sessions, direction);
        self.reassign_slots();
        self.render_dirty = true;
    }

    fn render(&mut self) {
        let offline = self.model.is_none();
        let filter = self.filter.clone();
        let allowed = self.config.herdr.sessions.clone();
        let active = active_endpoint(&self.directory);
        let agents = shown_agents(self.model.as_ref(), &allowed, &filter, active);
        let by_pane = agents
            .iter()
            .map(|(session, row)| {
                (
                    (row.endpoint_id.as_str(), row.agent.pane_id.as_str()),
                    (*session, *row),
                )
            })
            .collect::<HashMap<_, _>>();
        // The strip counts what is known now: not the agents of an endpoint
        // that is not online.
        let touch_agents = agents
            .iter()
            .filter(|(session, _)| session.connected)
            .map(|(_, agent)| *agent)
            .collect::<Vec<_>>();
        let multiple_sessions = self.connected_session_keys().len() > 1;
        let filter_label = self.filter_label();
        let now_ms = unix_millis();

        for device in self.devices.values_mut() {
            if device.config.role != DeviceRole::Dashboard || device.info.kind != Kind::Plus {
                continue;
            }
            let mut batch = FrameBatch::default();
            let (width, height) = device.info.kind.key_image_format().size;
            for key in 0..device.info.kind.key_count() {
                let slot = self.slots.get(key as usize).and_then(Option::as_ref);
                let entry = slot.and_then(|slot| {
                    by_pane
                        .get(&(slot.endpoint_id.as_str(), slot.pane_id.as_str()))
                        .copied()
                });
                let session = entry.map(|(session, _)| session);
                let agent = entry.map(|(_, row)| row);
                // An agent whose endpoint is not online keeps its key, in
                // the `unknown` colors: its status is stale and a press is
                // refused.
                let reachable = session.is_none_or(|session| session.connected);
                let status = agent.map(|row| key_status(row, reachable));
                let animated = matches!(status, Some("working" | "done" | "idle"));
                let label = agent.map(agent_label);
                // The row's own focus is the client's: one agent at most.
                let focused = agent.is_some_and(|row| row.focused);
                let session_badge = (self.filter == SessionFilter::All && multiple_sessions)
                    .then(|| session.map(session_label))
                    .flatten();
                let active_session =
                    session.is_some_and(|session| active == Some(session.key.as_str()));
                let signature = ButtonSignature {
                    offline,
                    pane_id: slot.map(|slot| slot.pane_id.clone()),
                    status: status.map(str::to_owned),
                    focused,
                    active_session,
                    title: label.as_ref().map(|label| label.title.clone()),
                    subtitle: (!animated)
                        .then(|| label.as_ref().map(|label| label.subtitle.clone()))
                        .flatten(),
                    session: session
                        .map(|session| session.key.clone())
                        .unwrap_or_default(),
                    session_badge: session_badge.map(str::to_owned),
                    tick: animation_tick(now_ms, status, key),
                    config_version: self.config_version,
                };
                if device.button_signatures.get(&key) == Some(&signature) {
                    continue;
                }
                let unreachable = agent.filter(|_| !reachable).map(|row| {
                    let mut row = row.clone();
                    row.agent.agent_status = key_status(&row, reachable).into();
                    row
                });
                match self.renderer.render_key(
                    width as u32,
                    height as u32,
                    unreachable.as_ref().or(agent),
                    focused,
                    active_session,
                    session_badge,
                    &self.config.states,
                    offline,
                    now_ms as f64 / 1_000.0,
                ) {
                    Ok(image) => {
                        batch.keys.insert(key, image);
                        device.button_signatures.insert(key, signature);
                    }
                    Err(error) => eprintln!("render failed: {error}"),
                }
            }

            if let Some((width, height)) = device.info.kind.lcd_strip_size() {
                let touch_signature = TouchSignature {
                    offline,
                    session: filter_label.clone(),
                    working: touch_agents
                        .iter()
                        .filter(|row| row.agent.agent_status.as_str() == "working")
                        .count(),
                    done: touch_agents
                        .iter()
                        .filter(|row| row.agent.agent_status.as_str() == "done")
                        .count(),
                    blocked: touch_agents
                        .iter()
                        .filter(|row| row.agent.agent_status.as_str() == "blocked")
                        .count(),
                    config_version: self.config_version,
                };
                if device.touch_signature.as_ref() != Some(&touch_signature) {
                    match self.renderer.render_touch_strip(
                        width as u32,
                        height as u32,
                        &filter_label,
                        &touch_agents,
                        &self.config.states,
                        offline,
                    ) {
                        Ok(image) => {
                            batch.lcd = Some(image);
                            device.touch_signature = Some(touch_signature);
                        }
                        Err(error) => eprintln!("render failed: {error}"),
                    }
                }
            }
            if !batch.keys.is_empty() || batch.lcd.is_some() {
                device.handle.submit(batch);
            }
        }
    }

    fn handle_input(&mut self, serial: &str, event: InputEvent) {
        let Some(device) = self.devices.get(serial) else {
            return;
        };
        let actions = match event {
            InputEvent::ButtonDown(index) => device
                .config
                .buttons
                .get(&index.to_string())
                .cloned()
                .map(|action| match action {
                    Action::FocusSlot { slot: None } => Action::FocusSlot { slot: Some(index) },
                    action => action,
                })
                .or_else(|| {
                    (device.config.role == DeviceRole::Dashboard)
                        .then_some(Action::FocusSlot { slot: Some(index) })
                })
                .into_iter()
                .collect(),
            InputEvent::EncoderDown(index) => device
                .config
                .encoders
                .get(&index.to_string())
                .and_then(|binding| binding.press.clone())
                .into_iter()
                .collect(),
            InputEvent::EncoderTwist { index, amount } => {
                let action = device
                    .config
                    .encoders
                    .get(&index.to_string())
                    .and_then(|binding| {
                        if amount > 0 {
                            binding.clockwise.clone()
                        } else {
                            binding.counterclockwise.clone()
                        }
                    });
                action
                    .into_iter()
                    .cycle()
                    .take(amount.unsigned_abs() as usize)
                    .collect()
            }
            InputEvent::TouchPress { x, long: false, .. } => {
                let width = device
                    .info
                    .kind
                    .lcd_strip_size()
                    .map(|size| size.0)
                    .unwrap_or(800);
                let section = ((x as usize * 4) / width).min(3);
                device
                    .config
                    .touch
                    .get(&section.to_string())
                    .cloned()
                    .into_iter()
                    .collect()
            }
            InputEvent::TouchPress { long: true, .. } => Vec::new(),
            InputEvent::TouchSwipe { from, to } => device
                .config
                .touch
                .get(if to.0 > from.0 {
                    "swipeRight"
                } else {
                    "swipeLeft"
                })
                .cloned()
                .into_iter()
                .collect(),
        };
        for action in actions {
            self.execute(action);
        }
    }

    fn execute(&mut self, action: Action) {
        if let Action::CycleSession { direction } = &action {
            self.cycle_filter(*direction);
            return;
        }
        let Some(actions) = self.workers.as_ref().map(|workers| workers.actions.clone()) else {
            return;
        };
        let name = action_name(&action);
        if let Some(command) = system_action_command(&action) {
            queue_action(&actions, name, command);
            return;
        }
        let slot = match &action {
            Action::FocusSlot { slot } => self.agent_for_slot(slot.unwrap_or(0) as usize),
            _ => None,
        };
        match plan_action(&action, &self.directory, slot) {
            Err(error) => eprintln!("{name} failed: {error}"),
            Ok(Planned::Jump(target)) => {
                let events = self.event_sender.clone();
                self.jumps.press(target, move |result| {
                    let result = jump_result(result);
                    let _ = events.send(Event::Action { name, result });
                });
            }
            Ok(Planned::Input { client, inputs }) => queue_action(
                &actions,
                name,
                ActionCommand::Input {
                    name,
                    client,
                    inputs,
                },
            ),
        }
    }

    fn shown_agents(&self) -> Vec<(&SessionState, &ClientReportAgent)> {
        shown_agents(
            self.model.as_ref(),
            &self.config.herdr.sessions,
            &self.filter,
            active_endpoint(&self.directory),
        )
    }

    /// Every shown agent keeps its slot; only one a jump can reach takes a
    /// free one. Without a followed client the slots wait as they are.
    fn reassign_slots(&mut self) {
        let Some(client) = self.directory.followed_client() else {
            return;
        };
        let mut owner = self.slots_client.take();
        let next = sticky_slots(
            &self.slots,
            &mut owner,
            client.key(),
            self.shown_agents().into_iter().map(|(_, row)| row),
            self.config.herdr.slot_count,
            |row| client.can_reach(row),
        );
        self.slots = next;
        self.slots_client = owner;
    }

    /// The followed client's endpoints that `herdr.sessions` allows.
    fn allowed_sessions(&self) -> impl Iterator<Item = &SessionState> {
        let allowed = &self.config.herdr.sessions;
        self.model
            .iter()
            .flat_map(|model| &model.sessions)
            .filter(move |session| allowed.is_empty() || allowed.contains(&session.key))
    }

    fn connected_session_keys(&self) -> Vec<String> {
        let mut sessions = self
            .allowed_sessions()
            .filter(|session| session.connected)
            .map(|session| session.key.clone())
            .collect::<Vec<_>>();
        sessions.sort();
        sessions.dedup();
        sessions
    }

    fn filter_label(&self) -> String {
        match &self.filter {
            SessionFilter::All => "all".into(),
            SessionFilter::Active => "active".into(),
            SessionFilter::Session(key) => self
                .model
                .as_ref()
                .and_then(|model| model.sessions.iter().find(|session| session.key == *key))
                .map(|session| session_label(session).to_owned())
                .unwrap_or_else(|| key.clone()),
        }
    }

    /// A filter on one endpoint falls back to all of them once that
    /// endpoint is gone: the followed client no longer reports it, or
    /// `herdr.sessions` no longer allows it. An endpoint that is only not
    /// online keeps its filter, and so does one that cannot be looked up
    /// because no client is followed or its daemon does not answer.
    fn normalize_filter(&mut self) {
        if self.directory.followed_client().is_none() {
            return;
        }
        let sessions = self
            .allowed_sessions()
            .map(|session| session.key.clone())
            .collect::<Vec<_>>();
        retain_filter(&mut self.filter, &sessions);
    }

    /// The agent behind a key, also while its endpoint is not online.
    fn agent_for_slot(&self, slot: usize) -> Option<&ClientReportAgent> {
        let key = self.slots.get(slot)?.as_ref()?;
        self.shown_agents()
            .into_iter()
            .map(|(_, row)| row)
            .find(|row| key.is(row))
    }
}

/// What a key shows as its agent's status: the reported one, or `unknown`
/// while the agent's endpoint is not online.
fn key_status(row: &ClientReportAgent, reachable: bool) -> &str {
    if reachable {
        row.agent.agent_status.as_str()
    } else {
        herdr_client::AgentStatus::UNKNOWN
    }
}

/// The agents of the endpoints the allow-list and the filter select, in the
/// client's order, each with its endpoint. An endpoint that is not online
/// is selected like any other: its agents keep their keys.
fn shown_agents<'a>(
    model: Option<&'a DisplayModel>,
    allowed: &[String],
    filter: &SessionFilter,
    active: Option<&str>,
) -> Vec<(&'a SessionState, &'a ClientReportAgent)> {
    let Some(model) = model else {
        return Vec::new();
    };
    model
        .agents
        .iter()
        .filter_map(|row| {
            let session = model
                .sessions
                .iter()
                .find(|session| session.key == row.endpoint_id)?;
            session_matches(session, allowed, filter, active).then_some((session, row))
        })
        .collect()
}

fn spawn_workers(events: mpsc::Sender<Event>, stopped: Arc<AtomicBool>) -> Workers {
    let animation_events = events.clone();
    let animation_stopped = Arc::clone(&stopped);
    let animation = thread::spawn(move || animation_worker(animation_events, animation_stopped));

    let (scan_stop, scan_stop_rx) = mpsc::channel();
    let scan_events = events.clone();
    let scan = thread::spawn(move || {
        let mut scanner = device::DeviceScanner::new();
        loop {
            let result = match &mut scanner {
                Ok(scanner) => scanner.scan(),
                Err(error) => Err(error.clone()),
            };
            if result.is_err() {
                scanner = device::DeviceScanner::new();
            }
            let _ = scan_events.send(Event::Scan(result));
            if scan_stop_rx.recv_timeout(SCAN_INTERVAL).is_ok() {
                break;
            }
        }
    });

    // Every local session is its own daemon; the followed client may be
    // attached to any of them, so all are followed.
    let focus = Focus::new(Arc::clone(&stopped));
    let follower = follow::spawn(
        sessions::discover,
        {
            let focus = focus.clone();
            let events = events.clone();
            move |directory| clients_changed(&focus, &events, directory)
        },
        |line| eprintln!("{line}"),
        stopped,
    );

    let (actions, action_rx) = mpsc::sync_channel(ACTION_QUEUE_CAPACITY);
    let action = thread::spawn({
        let focus = focus.clone();
        move || {
            action_worker(action_rx, events, &focus, &macos::post, &|| {
                thread::sleep(SUBMIT_SETTLE)
            })
        }
    });

    Workers {
        scan_stop,
        actions,
        focus,
        threads: vec![animation, scan, action, follower],
    }
}

/// The follower's callback, run on the follower's thread.
fn clients_changed(focus: &Focus, events: &mpsc::Sender<Event>, directory: Directory) -> bool {
    // Before the event, so a queued prompt sees a loss first.
    focus.follow(directory.followed_client());
    events.send(Event::Clients(directory)).is_ok()
}

/// Queues keyboard work for the action worker, which types one command at
/// a time. A command that finds the queue full is dropped.
fn queue_action(actions: &SyncSender<ActionCommand>, name: &str, command: ActionCommand) {
    match actions.try_send(command) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => eprintln!("{name} ignored: action queue full"),
        Err(TrySendError::Disconnected(_)) => {
            eprintln!("{name} failed: action worker is unavailable")
        }
    }
}

fn create_animation_timer() -> Result<libc::c_int, String> {
    let queue = unsafe { libc::kqueue() };
    if queue < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let change = libc::kevent {
        ident: 1,
        filter: libc::EVFILT_TIMER,
        flags: libc::EV_ADD | libc::EV_ENABLE,
        fflags: libc::NOTE_CRITICAL | libc::NOTE_USECONDS,
        data: ANIMATION_INTERVAL.as_micros() as isize,
        udata: std::ptr::null_mut(),
    };
    if unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) } == 0 {
        Ok(queue)
    } else {
        let error = std::io::Error::last_os_error().to_string();
        let _ = unsafe { libc::close(queue) };
        Err(error)
    }
}

fn animation_worker(events: mpsc::Sender<Event>, stopped: Arc<AtomicBool>) {
    let queue = match create_animation_timer() {
        Ok(queue) => queue,
        Err(error) => {
            let _ = events.send(Event::AnimationFailed(error));
            return;
        }
    };
    while !stopped.load(Ordering::Acquire) {
        let mut event = std::mem::MaybeUninit::uninit();
        let result = unsafe {
            libc::kevent(
                queue,
                std::ptr::null(),
                0,
                event.as_mut_ptr(),
                1,
                std::ptr::null(),
            )
        };
        if result == 1 {
            if events.send(Event::Animation).is_err() {
                break;
            }
        } else if result < 0
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
        {
            continue;
        } else {
            let error = if result < 0 {
                std::io::Error::last_os_error().to_string()
            } else {
                "timer stopped unexpectedly".into()
            };
            let _ = events.send(Event::AnimationFailed(error));
            break;
        }
    }
    let _ = unsafe { libc::close(queue) };
}

fn system_action_command(action: &Action) -> Option<ActionCommand> {
    match action {
        Action::SystemEnter => Some(ActionCommand::SystemKey(crate::system_input::Key::Enter)),
        Action::SystemKey { key } => Some(ActionCommand::SystemKey(*key)),
        _ => None,
    }
}

/// Whether `action` is sent as a macOS keyboard event.
fn posts_keyboard_event(action: &Action) -> bool {
    matches!(
        action,
        Action::SystemEnter | Action::SystemKey { .. } | Action::Prompt { .. } | Action::Submit
    )
}

fn has_keyboard_actions(config: &Config) -> bool {
    config
        .devices
        .iter()
        .filter(|device| device.enabled)
        .any(|device| {
            device
                .buttons
                .values()
                .chain(device.touch.values())
                .chain(
                    device
                        .encoders
                        .values()
                        .flat_map(|encoder| {
                            [
                                &encoder.press,
                                &encoder.clockwise,
                                &encoder.counterclockwise,
                            ]
                        })
                        .filter_map(Option::as_ref),
                )
                .any(posts_keyboard_event)
        })
}

fn report_keyboard_access(config: &Config) {
    if has_keyboard_actions(config) {
        println!(
            "system keyboard access: {}",
            if macos::permitted() {
                "ok"
            } else {
                "not granted; enable Herdr Deck in System Settings > Privacy & Security > Accessibility"
            }
        );
    }
}

/// What a finished jump reports. A jump a later press replaced reports
/// nothing: that later jump answers for itself.
fn jump_result<T>(result: Result<T, JumpError>) -> Result<(), String> {
    match result {
        Err(error) if !error.is_superseded() => Err(error.to_string()),
        _ => Ok(()),
    }
}

/// Runs queued keyboard work in order, until the queue's sender is dropped.
/// Once `focus` is stopped, what is still queued is dropped, and a prompt
/// being typed stops at its next keystroke.
fn action_worker(
    commands: Receiver<ActionCommand>,
    events: mpsc::Sender<Event>,
    focus: &Focus,
    post: &dyn Fn(Keystroke<'_>) -> Result<(), PostError>,
    settle: &dyn Fn(),
) {
    while let Ok(command) = commands.recv() {
        if focus.is_stopping() {
            break;
        }
        let (name, result) = match command {
            ActionCommand::Input {
                name,
                client,
                inputs,
            } => (
                name,
                type_inputs(&inputs, &client, focus, post, settle)
                    .map_err(|error| error.to_string()),
            ),
            ActionCommand::SystemKey(key) => (
                "system-key",
                crate::system_input::press(key).map_err(|error| error.to_string()),
            ),
        };
        if events.send(Event::Action { name, result }).is_err() {
            break;
        }
    }
}

/// The filter one step from `current` through all, active, and each of
/// `sessions`. A filter kept on an endpoint that is not among them, because
/// it is not online, has no place to step from: either direction leaves it
/// for all.
fn cycle_filter(
    current: &SessionFilter,
    sessions: &[String],
    direction: CycleDirection,
) -> SessionFilter {
    let mut filters = Vec::with_capacity(sessions.len() + 2);
    filters.push(SessionFilter::All);
    filters.push(SessionFilter::Active);
    filters.extend(sessions.iter().cloned().map(SessionFilter::Session));
    let Some(current) = filters.iter().position(|filter| filter == current) else {
        return SessionFilter::All;
    };
    let index = match direction {
        CycleDirection::Next => (current + 1) % filters.len(),
        CycleDirection::Previous => (current + filters.len() - 1) % filters.len(),
    };
    filters[index].clone()
}

fn retain_filter(filter: &mut SessionFilter, sessions: &[String]) {
    if let SessionFilter::Session(key) = filter
        && !sessions.contains(key)
    {
        *filter = SessionFilter::All;
    }
}

/// Whether the allow-list and the filter select `session`, online or not.
fn session_matches(
    session: &SessionState,
    allowed: &[String],
    filter: &SessionFilter,
    active: Option<&str>,
) -> bool {
    (allowed.is_empty() || allowed.contains(&session.key))
        && match filter {
            SessionFilter::All => true,
            SessionFilter::Active => active == Some(session.key.as_str()),
            SessionFilter::Session(key) => key == &session.key,
        }
}

fn session_label(session: &SessionState) -> &str {
    &session.name
}

fn supported(kind: Kind) -> bool {
    matches!(kind, Kind::Plus | Kind::Pedal)
}

fn identity(device: &DeviceInfo) -> DeviceIdentity {
    DeviceIdentity {
        model: device.model.clone(),
        serial_number: Some(device.serial_number.clone()),
    }
}

fn action_name(action: &Action) -> &'static str {
    match action {
        Action::FocusSlot { .. } => "focus-slot",
        Action::Prompt { .. } => "prompt",
        Action::Submit => "submit",
        Action::SystemEnter => "system-enter",
        Action::SystemKey { .. } => "system-key",
        Action::CycleSession { .. } => "cycle-session",
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn animation_tick(now_ms: u64, status: Option<&str>, button_index: u8) -> u64 {
    match status {
        Some("working") => now_ms.saturating_mul(WORKING_UPDATE_FPS) / 1_000,
        Some("done") => now_ms.saturating_mul(DONE_UPDATE_FPS) / 1_000,
        Some("idle") => {
            let phases = ANIMATION_TIMER_FPS * IDLE_INTERVAL_MS / 1_000;
            let offset_ms = (button_index as u64 % phases) * 1_000 / ANIMATION_TIMER_FPS;
            (now_ms + offset_ms) / IDLE_INTERVAL_MS
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_client::{ClientEndpointStatus, ClientInfo, ClientJumpError};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use crate::clients::tests::{client, daemon, directory, fixture_report};
    use crate::config::parse_config;

    /// Bounds every wait of a test that would otherwise hang.
    const WAIT: Duration = Duration::from_secs(10);

    /// The endpoint of the fixture's remote agent, which is the third key.
    const REMOTE: &str = "ssh:0123456789abcdef0123456789abcdef";

    /// A daemon with the shipped configuration, no devices and no worker
    /// threads. The receiver is the other end of its keyboard queue.
    fn deck() -> (Daemon, Receiver<ActionCommand>) {
        let config = serde_json::from_str(include_str!("../herdr-deck.json")).unwrap();
        let events = mpsc::channel();
        let (scan_stop, _) = mpsc::channel();
        let (actions, queued) = mpsc::sync_channel(ACTION_QUEUE_CAPACITY);
        let stopped = Arc::new(AtomicBool::new(false));
        let workers = Workers {
            scan_stop,
            actions,
            focus: Focus::new(Arc::clone(&stopped)),
            threads: Vec::new(),
        };
        let deck = Daemon::with_workers(
            "/nonexistent/herdr-deck.json".into(),
            parse_config(config).unwrap(),
            Renderer::new().unwrap(),
            workers,
            events,
            stopped,
        );
        (deck, queued)
    }

    /// The follower publishes `daemons` after what `deck` holds, and the
    /// event loop takes the event.
    fn publish(deck: &mut Daemon, daemons: Vec<herdr_client::follow::DaemonClients>) {
        let focus = deck.workers.as_ref().unwrap().focus.clone();
        let directory = deck.directory.updated(daemons);
        assert!(clients_changed(&focus, &deck.event_sender, directory));
        while let Ok(event) = deck.events.try_recv() {
            deck.handle_event(event);
        }
    }

    fn slot_panes(deck: &Daemon) -> Vec<Option<&str>> {
        deck.slots
            .iter()
            .map(|slot| slot.as_ref().map(|key| key.pane_id.as_str()))
            .collect()
    }

    /// What pressing the key of `slot` would do now.
    fn plan_slot(deck: &Daemon, slot: usize) -> Result<Planned, String> {
        plan_action(
            &Action::FocusSlot {
                slot: Some(slot as u8),
            },
            &deck.directory,
            deck.agent_for_slot(slot),
        )
    }

    #[test]
    fn executable_install_preserves_open_copy_and_survives_missing_source() {
        let directory = std::env::temp_dir().join(format!(
            "herdr-deck-install-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("build");
        let destination = directory.join("installed/bin/herdr-deck");
        fs::write(&source, b"old executable").unwrap();
        atomic_install(&source, &destination).unwrap();
        let mut running = fs::File::open(&destination).unwrap();
        fs::write(&source, b"new executable").unwrap();
        atomic_install(&source, &destination).unwrap();
        let mut old_bytes = Vec::new();
        running.read_to_end(&mut old_bytes).unwrap();
        assert_eq!(old_bytes, b"old executable");
        assert_eq!(fs::read(&destination).unwrap(), b"new executable");
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o755
        );
        atomic_install(&destination, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new executable");
        fs::remove_file(&source).unwrap();
        assert!(atomic_install(&source, &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"new executable");
        assert_eq!(
            fs::read_dir(destination.parent().unwrap()).unwrap().count(),
            1
        );
        fs::remove_dir_all(directory).unwrap();
    }

    fn session(key: &str, name: &str, connected: bool) -> SessionState {
        SessionState {
            key: key.into(),
            name: name.into(),
            connected,
        }
    }

    #[test]
    fn system_keys_create_commands_without_a_herdr_client() {
        use crate::system_input::Key;
        assert!(matches!(
            system_action_command(&Action::SystemEnter),
            Some(ActionCommand::SystemKey(Key::Enter))
        ));
        assert!(matches!(
            system_action_command(&Action::SystemKey { key: Key::F19 }),
            Some(ActionCommand::SystemKey(Key::F19))
        ));
        assert!(system_action_command(&Action::Submit).is_none());
    }

    #[test]
    fn idle_animation_staggers_updates_across_timer_ticks() {
        let before = (0..8)
            .map(|key| animation_tick(1_000, Some("idle"), key))
            .collect::<Vec<_>>();
        let after = (0..8)
            .map(|key| animation_tick(1_034, Some("idle"), key))
            .collect::<Vec<_>>();
        assert_eq!(
            before
                .iter()
                .zip(after)
                .filter(|(before, after)| **before != *after)
                .count(),
            1
        );
    }

    #[test]
    fn active_animations_use_their_requested_frame_rates() {
        assert_eq!(animation_tick(1_000, Some("working"), 0), 15);
        assert_eq!(animation_tick(1_066, Some("working"), 0), 15);
        assert_eq!(animation_tick(1_067, Some("working"), 0), 16);
        assert_eq!(animation_tick(1_000, Some("done"), 0), 10);
        assert_eq!(animation_tick(1_050, Some("done"), 0), 10);
        assert_eq!(animation_tick(1_100, Some("done"), 0), 11);
        assert_eq!(animation_tick(1_050, Some("unknown"), 0), 0);
    }

    #[test]
    fn animation_worker_emits_ticks() {
        let (events, receiver) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let worker = thread::spawn(move || animation_worker(events, worker_stopped));

        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(500)),
            Ok(Event::Animation)
        ));
        stopped.store(true, Ordering::Release);
        worker.join().unwrap();
    }

    #[test]
    fn filters_cycle_active_and_connected_allowed_sessions() {
        let alpha = session("local", "alpha", true);
        let beta = session("ssh:beta", "beta", true);
        let stopped = session("ssh:stopped", "stopped", false);
        assert!(session_matches(&alpha, &[], &SessionFilter::All, None));
        // An endpoint that is not online is selected like any other.
        assert!(session_matches(&stopped, &[], &SessionFilter::All, None));
        assert!(session_matches(
            &beta,
            &["ssh:beta".into()],
            &SessionFilter::All,
            None,
        ));
        assert!(!session_matches(
            &alpha,
            &["ssh:beta".into()],
            &SessionFilter::All,
            None,
        ));
        assert!(!session_matches(
            &alpha,
            &[],
            &SessionFilter::Session("ssh:beta".into()),
            None,
        ));
        assert!(session_matches(
            &alpha,
            &[],
            &SessionFilter::Active,
            Some("local"),
        ));
        assert!(!session_matches(
            &beta,
            &[],
            &SessionFilter::Active,
            Some("local"),
        ));
        assert!(!session_matches(
            &alpha,
            &["ssh:beta".into()],
            &SessionFilter::Active,
            Some("local"),
        ));
        assert!(!session_matches(&alpha, &[], &SessionFilter::Active, None,));

        let keys = vec!["local".into(), "ssh:beta".into()];
        let active_filter = cycle_filter(&SessionFilter::All, &keys, CycleDirection::Next);
        assert_eq!(active_filter, SessionFilter::Active);
        let alpha_filter = cycle_filter(&active_filter, &keys, CycleDirection::Next);
        assert_eq!(alpha_filter, SessionFilter::Session("local".into()));
        assert_eq!(
            cycle_filter(&alpha_filter, &keys, CycleDirection::Next),
            SessionFilter::Session("ssh:beta".into())
        );
        assert_eq!(
            cycle_filter(&SessionFilter::All, &keys, CycleDirection::Previous),
            SessionFilter::Session("ssh:beta".into())
        );
        // A filter kept on an endpoint that is not online is not in the
        // cycle: a press leaves it for all, not for all's neighbour.
        for direction in [CycleDirection::Next, CycleDirection::Previous] {
            assert_eq!(
                cycle_filter(
                    &SessionFilter::Session("ssh:stopped".into()),
                    &keys,
                    direction
                ),
                SessionFilter::All
            );
        }

        let mut vanished = SessionFilter::Session("ssh:beta".into());
        retain_filter(&mut vanished, &["local".into()]);
        assert_eq!(vanished, SessionFilter::All);
        let mut active_filter = SessionFilter::Active;
        retain_filter(&mut active_filter, &[]);
        assert_eq!(active_filter, SessionFilter::Active);
    }

    #[test]
    fn button_signature_tracks_session_indicators() {
        let plain = ButtonSignature {
            offline: false,
            pane_id: Some("w1:p1".into()),
            status: Some("blocked".into()),
            focused: false,
            active_session: false,
            title: Some("agent".into()),
            subtitle: Some("workspace".into()),
            session: "local/default".into(),
            session_badge: None,
            tick: 0,
            config_version: 0,
        };
        assert_ne!(
            plain,
            ButtonSignature {
                session_badge: Some("default".into()),
                ..plain.clone()
            }
        );
        assert_ne!(
            plain,
            ButtonSignature {
                active_session: true,
                ..plain.clone()
            }
        );
    }

    fn focused_directory() -> Directory {
        directory(vec![
            daemon("default", vec![client(3, Some(false))]),
            daemon("work", vec![client(7, Some(true))]),
        ])
    }

    fn focused_key(directory: &Directory) -> ClientKey {
        directory.followed_client().unwrap().key()
    }

    #[test]
    fn shown_agents_follow_the_report_the_allow_list_and_the_filter() {
        let model = project_model(&focused_directory());
        let panes = |allowed: &[String], filter: &SessionFilter, active| {
            shown_agents(model.as_ref(), allowed, filter, active)
                .into_iter()
                .map(|(session, row)| (session.name.clone(), row.agent.pane_id.clone()))
                .collect::<Vec<_>>()
        };
        let all = panes(&[], &SessionFilter::All, Some("local"));
        assert_eq!(
            all,
            [
                ("Local".to_owned(), "w1:p1".to_owned()),
                ("Local".to_owned(), "w2:p1".to_owned()),
                ("Build".to_owned(), "w1:p4".to_owned()),
                ("Local".to_owned(), "w2:p2".to_owned()),
            ]
        );
        // `herdr.sessions` lists the endpoint ids to show.
        let remote = "ssh:0123456789abcdef0123456789abcdef".to_owned();
        assert_eq!(
            panes(
                std::slice::from_ref(&remote),
                &SessionFilter::All,
                Some("local")
            ),
            [("Build".to_owned(), "w1:p4".to_owned())]
        );
        assert_eq!(panes(&[], &SessionFilter::Active, Some("local")).len(), 3);
        assert_eq!(
            panes(&[], &SessionFilter::Session(remote), Some("local")),
            [("Build".to_owned(), "w1:p4".to_owned())]
        );
        assert!(shown_agents(None, &[], &SessionFilter::All, None).is_empty());
    }

    #[test]
    fn a_reconnecting_endpoint_keeps_its_agents_keys_until_it_is_back() {
        let (mut deck, _queued) = deck();
        let mut focused = client(7, Some(true));
        publish(&mut deck, vec![daemon("work", vec![focused.clone()])]);
        assert_eq!(
            slot_panes(&deck)[..5],
            [
                Some("w1:p1"),
                Some("w2:p1"),
                Some("w1:p4"),
                Some("w2:p2"),
                None
            ]
        );
        let slots = deck.slots.clone();
        let jump = plan_slot(&deck, 2).unwrap();
        assert!(matches!(&jump, Planned::Jump(target) if target.endpoint_id == REMOTE));

        // The remote endpoint reconnects. The client still reports its
        // agent, and a new local one besides.
        let report = focused.report.as_mut().unwrap();
        report.endpoints[1].status = ClientEndpointStatus::RECONNECTING.into();
        let mut newcomer = report.agents[0].clone();
        newcomer.agent.pane_id = "w9:p9".into();
        newcomer.agent.priority_key = "0.0.0000".into();
        report.agents.push(newcomer);
        publish(&mut deck, vec![daemon("work", vec![focused.clone()])]);
        // The key is still the remote agent's: the newcomer got a free one.
        assert_eq!(deck.slots[..4], slots[..4]);
        assert_eq!(slot_panes(&deck)[4], Some("w9:p9"));
        // It is drawn as unavailable and left out of the touch strip, and a
        // press on it is refused.
        let held = deck.agent_for_slot(2).expect("the key is held");
        let (session, _) = deck
            .shown_agents()
            .into_iter()
            .find(|(_, row)| std::ptr::eq(*row, held))
            .unwrap();
        assert!(!session.connected);
        assert_eq!(key_status(held, session.connected), "unknown");
        assert_eq!(key_status(held, true), "working");
        assert_eq!(
            plan_slot(&deck, 2).unwrap_err(),
            "the agent's endpoint is not online"
        );
        assert!(matches!(plan_slot(&deck, 1), Ok(Planned::Jump(_))));

        // The endpoint is back: the key jumps to the same agent again.
        focused.report.as_mut().unwrap().endpoints[1].status = ClientEndpointStatus::ONLINE.into();
        publish(&mut deck, vec![daemon("work", vec![focused.clone()])]);
        assert_eq!(deck.slots[..4], slots[..4]);
        assert_eq!(plan_slot(&deck, 2).unwrap(), jump);

        // An agent that leaves the report while its endpoint reconnects
        // gives its key up.
        let report = focused.report.as_mut().unwrap();
        report.endpoints[1].status = ClientEndpointStatus::RECONNECTING.into();
        report.agents.retain(|row| row.endpoint_id != REMOTE);
        publish(&mut deck, vec![daemon("work", vec![focused])]);
        assert_eq!(slot_panes(&deck)[2], None);
    }

    #[test]
    fn a_filter_outlives_a_failed_listing_and_a_reconnecting_endpoint() {
        let (mut deck, _queued) = deck();
        let mut focused = client(7, Some(true));
        publish(&mut deck, vec![daemon("work", vec![focused.clone()])]);
        // All, active, `local`, then the remote endpoint.
        for _ in 0..3 {
            deck.execute(Action::CycleSession {
                direction: CycleDirection::Next,
            });
        }
        let filter = SessionFilter::Session(REMOTE.into());
        assert_eq!(deck.filter, filter);
        assert_eq!(slot_panes(&deck)[..2], [Some("w1:p4"), None]);
        let slots = deck.slots.clone();

        // The daemon fails a listing, with and without a client of another
        // daemon attached; then it answers again.
        let mut offline = daemon("work", vec![]);
        offline.state = DaemonState::Offline;
        let silent = ClientInfo {
            client_id: 9,
            report: None,
        };
        for daemons in [
            vec![offline.clone()],
            vec![offline, daemon("default", vec![silent])],
            vec![daemon("work", vec![focused.clone()])],
        ] {
            publish(&mut deck, daemons);
            assert_eq!(deck.filter, filter);
            assert_eq!(deck.slots, slots);
        }
        assert!(matches!(plan_slot(&deck, 0), Ok(Planned::Jump(_))));

        // The filtered endpoint reconnects: its agent keeps the key.
        let report = focused.report.as_mut().unwrap();
        report.endpoints[1].status = ClientEndpointStatus::RECONNECTING.into();
        publish(&mut deck, vec![daemon("work", vec![focused.clone()])]);
        assert_eq!(deck.filter, filter);
        assert_eq!(deck.slots, slots);
        assert_eq!(deck.filter_label(), "Build");

        // `herdr.sessions` stops allowing the endpoint: nothing is left for
        // the filter to select.
        deck.config.herdr.sessions = vec!["local".into()];
        deck.normalize_filter();
        assert_eq!(deck.filter, SessionFilter::All);
        deck.config.herdr.sessions.clear();

        // So it is once the client no longer reports the endpoint.
        deck.filter = filter;
        let report = focused.report.as_mut().unwrap();
        report.endpoints.remove(1);
        report.agents.retain(|row| row.endpoint_id != REMOTE);
        publish(&mut deck, vec![daemon("work", vec![focused])]);
        assert_eq!(deck.filter, SessionFilter::All);
        assert_eq!(
            slot_panes(&deck)[..4],
            [Some("w1:p1"), Some("w2:p1"), Some("w2:p2"), None]
        );
    }

    /// A socket in the place of the followed client's daemon. Nothing here
    /// touches a real Herdr socket.
    struct JumpSocket {
        directory: PathBuf,
        listener: UnixListener,
    }

    impl JumpSocket {
        fn bind() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let directory = std::env::temp_dir().join(format!(
                "hd-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&directory).unwrap();
            let listener = UnixListener::bind(directory.join("s")).unwrap();
            listener.set_nonblocking(true).unwrap();
            Self {
                directory,
                listener,
            }
        }

        fn path(&self) -> PathBuf {
            self.directory.join("s")
        }

        /// The next request and the connection it arrived on.
        fn request(&self) -> (UnixStream, serde_json::Value) {
            let deadline = Instant::now() + WAIT;
            let stream = loop {
                match self.listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(_) => {
                        assert!(Instant::now() < deadline, "no request arrived");
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(WAIT)).unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            (stream, serde_json::from_str(&line).unwrap())
        }
    }

    impl Drop for JumpSocket {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn a_slot_press_sends_the_slots_jump_and_reports_its_answer() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../sdk/rust/tests/fixtures/api-client-jump-v1.json"
        ))
        .unwrap();
        let socket = JumpSocket::bind();
        let (mut deck, _queued) = deck();
        let mut work = daemon("work", vec![client(7, Some(true))]);
        work.socket_path = socket.path();
        publish(
            &mut deck,
            vec![daemon("default", vec![client(3, Some(false))]), work],
        );
        let outcome = |deck: &Daemon| match deck.events.recv_timeout(WAIT) {
            Ok(Event::Action { name, result }) => (name, result),
            _ => panic!("expected the outcome of an action"),
        };

        // An empty key sends nothing; the request below is the first.
        deck.execute(Action::FocusSlot { slot: Some(6) });
        // The third key is the remote agent: its endpoint, that endpoint's
        // boot and its pane go to the daemon the followed client is
        // attached to, as the fork's fixture has them.
        deck.execute(Action::FocusSlot { slot: Some(2) });
        let (mut stream, request) = socket.request();
        assert_eq!(request["method"], "client.jump");
        assert_eq!(request["params"], fixture["request"]["params"]);
        let mut response = fixture["response"].clone();
        response["id"] = request["id"].clone();
        writeln!(stream, "{response}").unwrap();
        assert_eq!(outcome(&deck), ("focus-slot", Ok(())));

        // The daemon's refusal is logged in the words of the typed error.
        deck.execute(Action::FocusSlot { slot: Some(0) });
        let (mut stream, request) = socket.request();
        assert_eq!(request["params"]["pane_id"], "w1:p1");
        let refusal = json!({
            "id": request["id"],
            "error": {"code": "stale_boot", "message": "endpoint boot changed"},
        });
        writeln!(stream, "{refusal}").unwrap();
        assert_eq!(
            outcome(&deck),
            (
                "focus-slot",
                Err("jump to local/w1:p1: the endpoint now runs another boot".into())
            )
        );
        // Each press reported once, and the empty key not at all.
        assert!(deck.events.try_recv().is_err());
    }

    #[test]
    fn keyboard_work_beyond_the_queue_is_ignored() {
        let (mut deck, queued) = deck();
        publish(&mut deck, vec![daemon("work", vec![client(7, Some(true))])]);
        // Nothing takes the work, as while a long prompt is typed. Every
        // press returns at once.
        for _ in 0..ACTION_QUEUE_CAPACITY + 4 {
            deck.execute(Action::Submit);
        }
        deck.execute(Action::SystemEnter);
        assert_eq!(queued.try_iter().count(), ACTION_QUEUE_CAPACITY);
        // With room again, work is taken again.
        deck.execute(Action::Submit);
        assert!(matches!(
            queued.try_recv(),
            Ok(ActionCommand::Input { name: "submit", .. })
        ));
    }

    #[test]
    fn nothing_is_planned_without_a_followed_client() {
        let row = fixture_report().agents[0].clone();
        let actions = [
            Action::FocusSlot { slot: Some(0) },
            Action::Prompt {
                text: "/model".into(),
                submit: None,
            },
            Action::Submit,
        ];
        let silent = ClientInfo {
            client_id: 8,
            report: None,
        };
        // The followed client detached and no other has sent a report.
        let detached = focused_directory().updated(vec![daemon("work", vec![silent.clone()])]);
        for directory in [
            directory(vec![]),
            directory(vec![daemon("default", vec![])]),
            directory(vec![daemon("default", vec![silent])]),
            detached,
        ] {
            for action in &actions {
                assert_eq!(
                    plan_action(action, &directory, Some(&row)).unwrap_err(),
                    "no followed Herdr client"
                );
            }
        }
    }

    #[test]
    fn another_app_in_front_still_jumps_but_types_nothing() {
        let front = focused_directory();
        let behind = front.updated(vec![
            daemon("default", vec![client(3, Some(false))]),
            daemon("work", vec![client(7, Some(false))]),
        ]);
        let row = fixture_report().agents[0].clone();
        let jump =
            |directory| plan_action(&Action::FocusSlot { slot: Some(0) }, directory, Some(&row));
        assert!(matches!(jump(&behind), Ok(Planned::Jump(_))));
        assert_eq!(jump(&behind), jump(&front));
        for action in [
            Action::Prompt {
                text: "/model".into(),
                submit: None,
            },
            Action::Submit,
        ] {
            assert_eq!(
                plan_action(&action, &behind, None).unwrap_err(),
                Refusal::NotFocused.to_string()
            );
            assert!(plan_action(&action, &front, None).is_ok());
        }
    }

    #[test]
    fn a_client_whose_focus_is_unknown_is_jumped_but_never_typed_into() {
        // A TUI that has not seen a focus event from its terminal yet.
        let fresh = directory(vec![daemon("work", vec![client(7, None)])]);
        let row = fixture_report().agents[0].clone();
        assert!(matches!(
            plan_action(&Action::FocusSlot { slot: Some(0) }, &fresh, Some(&row)),
            Ok(Planned::Jump(_))
        ));
        for action in [
            Action::Prompt {
                text: "/model".into(),
                submit: None,
            },
            Action::Submit,
        ] {
            assert_eq!(
                plan_action(&action, &fresh, None).unwrap_err(),
                Refusal::NotFocused.to_string()
            );
        }
    }

    #[test]
    fn a_replaced_jump_reports_nothing_and_a_failed_one_its_error() {
        let failed = |source| JumpError::Failed {
            endpoint_id: "local".into(),
            pane_id: "w1:p1".into(),
            source,
        };
        assert_eq!(jump_result(Ok(())), Ok(()));
        assert_eq!(
            jump_result::<()>(Err(failed(ClientJumpError::Superseded))),
            Ok(())
        );
        assert_eq!(jump_result::<()>(Err(JumpError::Replaced)), Ok(()));
        assert_eq!(jump_result::<()>(Err(JumpError::Overtaken)), Ok(()));
        assert_eq!(
            jump_result::<()>(Err(failed(ClientJumpError::Timeout))),
            Err(format!("jump to local/w1:p1: {}", ClientJumpError::Timeout))
        );
        assert_eq!(
            jump_result::<()>(Err(Refusal::NotFollowed.into())),
            Err(Refusal::NotFollowed.to_string())
        );
    }

    #[test]
    fn a_slot_jumps_with_the_rows_endpoint_its_boot_and_the_pane() {
        let directory = focused_directory();
        let report = fixture_report();
        // The remote agent: its endpoint need not be the one presented.
        let Planned::Jump(target) = plan_action(
            &Action::FocusSlot { slot: Some(2) },
            &directory,
            Some(&report.agents[2]),
        )
        .unwrap() else {
            panic!("expected a jump");
        };
        assert_eq!(target.client, focused_key(&directory));
        assert_eq!(target.client.client_id, 7);
        assert_eq!(
            target.client.socket_path,
            Path::new("/run/herdr/work/herdr.sock")
        );
        assert_eq!(target.endpoint_id, "ssh:0123456789abcdef0123456789abcdef");
        assert_eq!(target.boot_id, "boot-b");
        assert_eq!(target.pane_id, "w1:p4");

        assert_eq!(
            plan_action(&Action::FocusSlot { slot: Some(5) }, &directory, None).unwrap_err(),
            "agent slot is empty"
        );
        let mut offline = report.agents[2].clone();
        offline.endpoint_id = "ssh:fedcba9876543210fedcba9876543210".into();
        assert_eq!(
            plan_action(
                &Action::FocusSlot { slot: Some(2) },
                &directory,
                Some(&offline)
            )
            .unwrap_err(),
            "the agent's endpoint is not online"
        );
    }

    #[test]
    fn prompts_and_submits_become_keyboard_events_for_the_followed_client() {
        let directory = focused_directory();
        let inputs = |action| match plan_action(&action, &directory, None).unwrap() {
            Planned::Input { client, inputs } => {
                assert_eq!(client, focused_key(&directory));
                inputs
            }
            Planned::Jump(_) => panic!("expected input"),
        };
        assert_eq!(
            inputs(Action::Prompt {
                text: "/model".into(),
                submit: None
            }),
            [OsInput::Text("/model".into()), OsInput::Enter]
        );
        assert_eq!(
            inputs(Action::Prompt {
                text: "draft".into(),
                submit: Some(false)
            }),
            [OsInput::Text("draft".into())]
        );
        assert_eq!(inputs(Action::Submit), [OsInput::Enter]);
    }

    /// Runs queued commands through the action worker with a recording sink
    /// instead of the OS, and returns what was posted and each result.
    /// `during_settle` runs while a prompt settles.
    fn run_commands(
        commands: Vec<ActionCommand>,
        focus: &Focus,
        during_settle: &dyn Fn(),
    ) -> (Vec<String>, Vec<Result<(), String>>) {
        let (sender, receiver) = mpsc::sync_channel(commands.len());
        let (events, results) = mpsc::channel();
        for command in commands {
            sender.send(command).unwrap();
        }
        // The worker ends when its queue is empty and has no sender.
        drop(sender);
        let posted = Mutex::new(Vec::new());
        let record = |line: String| posted.lock().unwrap().push(line);
        action_worker(
            receiver,
            events,
            focus,
            &|keystroke| {
                record(match keystroke {
                    Keystroke::Text(units) => format!("text {}", String::from_utf16_lossy(units)),
                    Keystroke::Enter => "enter".into(),
                });
                Ok(())
            },
            &|| {
                record("settle".into());
                during_settle();
            },
        );
        let results = results
            .try_iter()
            .map(|event| match event {
                Event::Action { result, .. } => result,
                _ => panic!("expected an action result"),
            })
            .collect();
        (posted.into_inner().unwrap(), results)
    }

    fn input_command(client: &ClientKey, inputs: Vec<OsInput>) -> ActionCommand {
        ActionCommand::Input {
            name: "prompt",
            client: client.clone(),
            inputs,
        }
    }

    /// `client` is followed and its window has focus.
    fn focused_on(client: &ClientKey) -> Focus {
        let focus = Focus::new(Arc::default());
        focus.set(Some(client.clone()), true);
        focus
    }

    #[test]
    fn a_prompt_types_settles_and_presses_enter_while_its_client_has_focus() {
        let key = focused_key(&focused_directory());
        let focus = focused_on(&key);
        let (posted, results) = run_commands(
            vec![
                input_command(&key, OsInput::prompt("/model", true)),
                input_command(&key, OsInput::prompt("draft", false)),
                input_command(&key, vec![OsInput::Enter]),
            ],
            &focus,
            &|| {},
        );
        assert_eq!(
            posted,
            ["text /model", "settle", "enter", "text draft", "enter"]
        );
        assert_eq!(results, [Ok(()), Ok(()), Ok(())]);

        // By the time the input is dispatched, another client is followed,
        // or the captured one is behind another app.
        let mut other = key.clone();
        other.client_id = 8;
        let behind = Focus::new(Arc::default());
        behind.set(Some(key.clone()), false);
        for (focus, refusal) in [
            (focused_on(&other), Refusal::NotFollowed),
            (behind, Refusal::NotFocused),
        ] {
            let (posted, results) = run_commands(
                vec![input_command(&key, OsInput::prompt("/model", true))],
                &focus,
                &|| {},
            );
            assert!(posted.is_empty(), "{posted:?}");
            assert_eq!(results, [Err(refusal.to_string())]);
        }

        // The window loses focus while the text settles: no Enter.
        let focus = focused_on(&key);
        let (posted, results) = run_commands(
            vec![input_command(&key, OsInput::prompt("/model", true))],
            &focus,
            &|| focus.set(Some(key.clone()), false),
        );
        assert_eq!(posted, ["text /model", "settle"]);
        assert_eq!(results, [Err(Refusal::NotFocused.to_string())]);
    }

    #[test]
    fn stopping_drops_queued_input_and_the_rest_of_a_prompt() {
        let key = focused_key(&focused_directory());
        let focus = focused_on(&key);
        // Shutdown starts while the first prompt settles, with more queued.
        // (A queued system key is dropped by the same check; it is left out
        // here because it would post to the OS, not to the recorder.)
        let (posted, results) = run_commands(
            vec![
                input_command(&key, OsInput::prompt("/model", true)),
                input_command(&key, OsInput::prompt("/copy", true)),
                input_command(&key, vec![OsInput::Enter]),
            ],
            &focus,
            &|| focus.stop(),
        );
        assert_eq!(posted, ["text /model", "settle"]);
        assert_eq!(results, [Err(Refusal::Stopping.to_string())]);
    }

    #[test]
    fn stopping_the_workers_forgets_the_followed_client() {
        let key = focused_key(&focused_directory());
        let focus = focused_on(&key);
        let (scan_stop, _scan) = mpsc::channel();
        let (actions, _actions) = mpsc::sync_channel(1);
        Workers {
            scan_stop,
            actions,
            focus: focus.clone(),
            threads: Vec::new(),
        }
        .stop();
        assert_eq!(focus.check_followed(&key), Err(Refusal::Stopping));
        // A report the follower delivers late cannot bring it back.
        focus.set(Some(key.clone()), true);
        assert_eq!(focus.check_followed(&key), Err(Refusal::Stopping));
        assert_eq!(focus.check_focused(&key), Err(Refusal::Stopping));
    }

    #[test]
    fn the_service_is_given_the_herdr_executable_it_lists_sessions_with() {
        let plist = service_plist(
            Path::new("/Users/me/.local/share/herdr-deck/bin/herdr-deck"),
            Path::new("/Users/me/.config/herdr-deck/config.json"),
            Path::new("/Users/me/.local/share/herdr-deck"),
            Path::new("/Users/me/Library/Logs/herdr-deck.log"),
            Path::new("/opt/a&b/bin/herdr"),
        );
        assert!(plist.contains(
            "<key>EnvironmentVariables</key><dict><key>HERDR_BIN_PATH</key><string>/opt/a&amp;b/bin/herdr</string></dict>"
        ));
        assert!(plist.contains(
            "<array><string>/Users/me/.local/share/herdr-deck/bin/herdr-deck</string><string>run</string><string>/Users/me/.config/herdr-deck/config.json</string></array>"
        ));
        assert!(!plist.contains("HERDR_CLIENT_API_DIR"));
    }
}
