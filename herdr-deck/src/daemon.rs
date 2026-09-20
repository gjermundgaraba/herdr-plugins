use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::frontends::{self, Model};
use crate::routing::{DisplayModel, SessionState};
use elgato_streamdeck::info::Kind;
use herdr_client::AgentInfo;
use herdr_frontend::{self as frontend, FrontendClient, Input, NavigationTarget};
use serde_json::json;
use signal_hook::consts::{SIGINT, SIGTERM};

use crate::config::{
    Action, AgentStatus, Config, CycleDirection, DeviceConfig, DeviceIdentity, DeviceRole,
    Direction, config_path, escape_xml, load_config, select_device_config,
};
use crate::device::{self, DeviceEvent, DeviceHandle, DeviceInfo, FrameBatch, InputEvent};
use crate::render::Renderer;
use crate::routing::{ActionRoute, RoutingState, active_endpoint, project_model};
use crate::slots::{SlotAgent, SlotKey, agent_label, assign_slots, resize_slots};

const ANIMATION_TIMER_FPS: u64 = 30;
const ANIMATION_INTERVAL: Duration = Duration::from_micros(1_000_000 / ANIMATION_TIMER_FPS);
const WORKING_UPDATE_FPS: u64 = 15;
const DONE_UPDATE_FPS: u64 = 10;
const IDLE_INTERVAL_MS: u64 = 200;
const CONFIG_INTERVAL: Duration = Duration::from_secs(1);
const SCAN_INTERVAL: Duration = Duration::from_secs(2);
const ACTION_TIMEOUT: Duration = Duration::from_secs(5);

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
        "Usage: herdr-deck <command>\n\n  devices                   List every visible Elgato device (Plus and Pedal are managed)\n  check [config]            Validate configuration\n  doctor [config]           Check config, Herdr frontends, and device visibility\n  frames <state> <dir> [config]  Write one animation loop as JPEG key frames\n  push [source] [target]    Atomically install configuration\n  run [config]              Run the direct-HID daemon\n  install-service [plist] [config]  Install and start the launchd user service\n  uninstall-service [plist] Stop and remove the launchd user service\n"
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
    let agent: AgentInfo = serde_json::from_value(json!({
        "terminal_id": "preview", "agent": "claude", "title": "preview",
        "agent_status": state.as_str(), "workspace_id": "preview",
        "tab_id": "preview", "pane_id": "preview", "focused": false, "revision": 1,
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
                None,
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
    let paths = frontend::discover(&frontend::directory());
    let clients = paths
        .into_iter()
        .filter_map(|path| {
            match FrontendClient::connect(&path)
                .with_timeout(Duration::from_secs(2))
                .snapshot()
            {
                Ok(snapshot) => Some(frontends::ClientState {
                    socket_path: path,
                    snapshot,
                }),
                Err(error) => {
                    eprintln!("{}: {error}", path.display());
                    None
                }
            }
        })
        .collect::<Vec<_>>();
    if clients.is_empty() {
        return Err("no compatible Herdr frontends available".into());
    }
    let model = Model { clients };
    println!(
        "frontends: {} connected; unique focused client: {}",
        model.clients.len(),
        model.foremost_client().is_some()
    );
    let model = project_model(&model);
    let connected = model
        .sessions
        .iter()
        .filter(|session| {
            session.connected
                && (config.herdr.sessions.is_empty()
                    || config.herdr.sessions.contains(&session.key))
        })
        .count();
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
    println!("frontend endpoints: {connected} available in focused-client scope");
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
    let discovery_env = discovery_environment(std::env::var_os("HERDR_CLIENT_API_DIR").as_deref());
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>dev.herdr.deck</string>\n<key>ProgramArguments</key><array><string>{}</string><string>run</string><string>{}</string></array>\n<key>WorkingDirectory</key><string>{}</string>\n{discovery_env}<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>\n<key>ThrottleInterval</key><integer>5</integer>\n<key>StandardOutPath</key><string>{}</string>\n<key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n",
        escape_xml(&executable.to_string_lossy()),
        escape_xml(&config.to_string_lossy()),
        escape_xml(&working_directory.to_string_lossy()),
        escape_xml(&log.to_string_lossy()),
        escape_xml(&log.to_string_lossy()),
    );
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
    terminal_id: Option<String>,
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
    Frontends(RoutingState),
    Device(DeviceEvent),
    Action {
        name: &'static str,
        result: Result<(), String>,
    },
}

enum ActionCommand {
    SystemKey(crate::system_input::Key),
    Call {
        name: &'static str,
        route: Box<ActionRoute>,
        request: FrontendRequest,
    },
    Stop,
}

/// One request sent through the captured TUI route.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FrontendRequest {
    /// Focus the captured pane, on any endpoint of the TUI.
    Navigate,
    /// Ordinary input following the TUI's own focus, which the route pins to
    /// the captured source pane.
    Input(Input),
    /// An endpoint method advertised on the TUI's client command lane.
    Call {
        method: &'static str,
        params: serde_json::Value,
    },
}

struct Workers {
    scan_stop: mpsc::Sender<()>,
    actions: mpsc::Sender<ActionCommand>,
    threads: Vec<JoinHandle<()>>,
}

impl Workers {
    fn stop(self) {
        let _ = self.scan_stop.send(());
        let _ = self.actions.send(ActionCommand::Stop);
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
    model: Option<DisplayModel>,
    routing: RoutingState,
    slots: Vec<Option<SlotKey>>,
    devices: HashMap<String, ManagedDevice>,
    visible: Vec<DeviceInfo>,
    renderer: Renderer,
    events: Receiver<Event>,
    event_sender: mpsc::Sender<Event>,
    workers: Option<Workers>,
    stopped: Arc<AtomicBool>,
    render_dirty: bool,
    failure: Option<String>,
}

impl Daemon {
    fn new(path: PathBuf) -> Result<Self, String> {
        let config = load_config(&path)?;
        let config_modified = fs::metadata(&path).and_then(|value| value.modified()).ok();
        let renderer = Renderer::new().map_err(|error| error.to_string())?;
        let stopped = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(SIGINT, Arc::clone(&stopped))
            .and_then(|_| signal_hook::flag::register(SIGTERM, Arc::clone(&stopped)))
            .map_err(|error| error.to_string())?;
        let (event_sender, events) = mpsc::channel();
        let workers = spawn_workers(event_sender.clone(), Arc::clone(&stopped));
        Ok(Self {
            path,
            filter: SessionFilter::All,
            slots: vec![None; config.herdr.slot_count],
            config,
            config_modified,
            config_version: 0,
            model: None,
            routing: RoutingState::default(),
            devices: HashMap::new(),
            visible: Vec::new(),
            renderer,
            events,
            event_sender,
            workers: Some(workers),
            stopped,
            render_dirty: true,
            failure: None,
        })
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
                if self.visible_agents().any(|(_, agent)| {
                    matches!(agent.agent_status.as_str(), "working" | "done" | "idle")
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
            Event::Frontends(routing) => {
                if self.model.is_some() && routing.model.is_none() {
                    eprintln!("Herdr frontends unavailable");
                }
                self.routing = routing;
                self.model = self.routing.model.as_ref().map(project_model);
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
        let active = self.routing.model.as_ref().and_then(active_endpoint);
        let agents = self
            .model
            .iter()
            .flat_map(|model| &model.sessions)
            .filter(|session| session_matches(session, &allowed, &filter, active))
            .flat_map(|session| session.agents.iter().map(move |agent| (session, agent)))
            .collect::<Vec<_>>();
        let by_terminal = agents
            .iter()
            .map(|(session, agent)| {
                (
                    (session.key.as_str(), agent.terminal_id.as_str()),
                    (*session, *agent),
                )
            })
            .collect::<HashMap<_, _>>();
        let touch_agents = agents.iter().map(|(_, agent)| *agent).collect::<Vec<_>>();
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
                    by_terminal
                        .get(&(slot.session.as_str(), slot.terminal_id.as_str()))
                        .copied()
                });
                let session = entry.map(|(session, _)| session);
                let agent = entry.map(|(_, agent)| agent);
                let workspace =
                    entry.and_then(|(session, agent)| workspace_for_agent(session, agent));
                let status = agent.map(|agent| agent.agent_status.as_str());
                let animated = matches!(status, Some("working" | "done" | "idle"));
                let label = agent.map(|agent| agent_label(agent, workspace));
                let session_badge = (self.filter == SessionFilter::All && multiple_sessions)
                    .then(|| session.map(session_label))
                    .flatten();
                let active_session =
                    session.is_some_and(|session| active == Some(session.key.as_str()));
                let signature = ButtonSignature {
                    offline,
                    terminal_id: slot.map(|slot| slot.terminal_id.clone()),
                    status: status.map(str::to_owned),
                    focused: agent.is_some_and(|agent| agent.focused),
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
                match self.renderer.render_key(
                    width as u32,
                    height as u32,
                    agent,
                    workspace,
                    agent.is_some_and(|agent| agent.focused),
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
                        .filter(|agent| agent.agent_status.as_str() == "working")
                        .count(),
                    done: touch_agents
                        .iter()
                        .filter(|agent| agent.agent_status.as_str() == "done")
                        .count(),
                    blocked: touch_agents
                        .iter()
                        .filter(|agent| agent.agent_status.as_str() == "blocked")
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
        if let Some(command) = system_action_command(&action) {
            if actions.send(command).is_err() {
                eprintln!(
                    "{} failed: action worker is unavailable",
                    action_name(&action)
                );
            }
            return;
        }
        let action_name = action_name(&action);
        let target = match &action {
            Action::FocusSlot { slot } => {
                let Some(target) = self.agent_for_slot(slot.unwrap_or(0) as usize) else {
                    eprintln!("focus-slot failed: agent slot is empty");
                    return;
                };
                Some(target)
            }
            _ => None,
        };
        let explicit = match &self.filter {
            SessionFilter::Session(key) => Some(key.as_str()),
            _ => None,
        };
        let route = match self
            .routing
            .capture(&self.config.herdr.sessions, explicit, target)
        {
            Ok(route) => route,
            Err(error) => {
                eprintln!("{action_name} failed: {error}");
                return;
            }
        };
        let command = action_command(action_name, route, action);
        if actions.send(command).is_err() {
            eprintln!("{action_name} failed: action worker is unavailable");
        }
    }

    fn visible_sessions(&self) -> impl Iterator<Item = &SessionState> {
        self.model
            .iter()
            .flat_map(|model| &model.sessions)
            .filter(|session| {
                session_matches(
                    session,
                    &self.config.herdr.sessions,
                    &self.filter,
                    self.routing.model.as_ref().and_then(active_endpoint),
                )
            })
    }

    fn visible_agents(&self) -> impl Iterator<Item = (&SessionState, &AgentInfo)> {
        self.visible_sessions()
            .flat_map(|session| session.agents.iter().map(move |agent| (session, agent)))
    }

    fn reassign_slots(&mut self) {
        let next = {
            let agents = self
                .visible_agents()
                .map(|(session, agent)| SlotAgent {
                    key: SlotKey {
                        session: session.key.clone(),
                        terminal_id: agent.terminal_id.clone(),
                    },
                    agent,
                })
                .collect::<Vec<_>>();
            assign_slots(&self.slots, &agents, self.config.herdr.slot_count)
        };
        self.slots = next;
    }

    fn connected_session_keys(&self) -> Vec<String> {
        let mut sessions = self
            .model
            .iter()
            .flat_map(|model| &model.sessions)
            .filter(|session| {
                session.connected
                    && (self.config.herdr.sessions.is_empty()
                        || self.config.herdr.sessions.contains(&session.key))
            })
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

    fn normalize_filter(&mut self) {
        let sessions = self.connected_session_keys();
        retain_filter(&mut self.filter, &sessions);
    }

    fn agent_for_slot(&self, slot: usize) -> Option<(&str, &AgentInfo)> {
        let key = self.slots.get(slot)?.as_ref()?;
        let session = self
            .model
            .as_ref()?
            .sessions
            .iter()
            .find(|session| session.key == key.session)?;
        let agent = session
            .agents
            .iter()
            .find(|agent| agent.terminal_id == key.terminal_id)?;
        Some((&session.key, agent))
    }
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

    let shared_routing = Arc::new(Mutex::new(RoutingState::default()));
    let frontend_routing = Arc::clone(&shared_routing);
    let frontend_events = events.clone();
    let frontend_worker = frontends::spawn_updates(
        frontend::directory(),
        move |clients| {
            let mut routing = frontend_routing.lock().unwrap_or_else(|e| e.into_inner());
            if routing
                .model
                .as_ref()
                .map(|m| &m.clients)
                .is_some_and(|old| *old == clients)
                || (routing.model.is_none() && clients.is_empty())
            {
                return true;
            }
            routing.update(clients);
            frontend_events
                .send(Event::Frontends(routing.clone()))
                .is_ok()
        },
        stopped,
    );

    let (actions, action_rx) = mpsc::channel();
    let action = thread::spawn(move || action_worker(action_rx, events, shared_routing));

    Workers {
        scan_stop,
        actions,
        threads: vec![animation, scan, action, frontend_worker],
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

fn has_system_actions(config: &Config) -> bool {
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
                .any(|action| system_action_command(action).is_some())
        })
}

fn report_keyboard_access(config: &Config) {
    if has_system_actions(config) {
        println!(
            "system keyboard access: {}",
            if crate::system_input::permitted() {
                "ok"
            } else {
                "not granted; enable Herdr Deck in System Settings > Privacy & Security > Accessibility"
            }
        );
    }
}

fn action_worker(
    commands: Receiver<ActionCommand>,
    events: mpsc::Sender<Event>,
    routing: Arc<Mutex<RoutingState>>,
) {
    while let Ok(command) = commands.recv() {
        let (name, result) = match command {
            ActionCommand::Call {
                name,
                route,
                request,
            } => {
                let client = route.client_route.client().with_timeout(ACTION_TIMEOUT);
                let validate = || {
                    routing
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .validate(&route)
                };
                (
                    name,
                    execute_frontend_call(&client, &route, &request, &validate).map(|_| ()),
                )
            }
            ActionCommand::SystemKey(key) => ("system-key", crate::system_input::press(key)),
            ActionCommand::Stop => break,
        };
        if events.send(Event::Action { name, result }).is_err() {
            break;
        }
    }
}

fn action_command(name: &'static str, route: ActionRoute, action: Action) -> ActionCommand {
    let pane_id = &route.target.pane_id;
    let request = match action {
        Action::FocusSlot { .. } => FrontendRequest::Navigate,
        Action::FocusPane { direction } => FrontendRequest::Call {
            method: "pane.focus_direction",
            params: json!({ "pane_id": pane_id, "direction": direction_name(direction) }),
        },
        Action::SendKeys { keys } => FrontendRequest::Input(Input::Keys(keys)),
        Action::Prompt {
            text,
            submit: Some(false),
        } => FrontendRequest::Input(Input::Text(text)),
        Action::Prompt { text, .. } => FrontendRequest::Call {
            method: "agent.prompt",
            params: json!({ "target": pane_id, "text": text }),
        },
        Action::Submit => FrontendRequest::Input(Input::Keys(vec!["enter".into()])),
        Action::CycleSession { .. } | Action::SystemEnter | Action::SystemKey { .. } => {
            unreachable!()
        }
    };
    ActionCommand::Call {
        name,
        route: Box::new(route),
        request,
    }
}

fn cycle_filter(
    current: &SessionFilter,
    sessions: &[String],
    direction: CycleDirection,
) -> SessionFilter {
    let mut filters = Vec::with_capacity(sessions.len() + 2);
    filters.push(SessionFilter::All);
    filters.push(SessionFilter::Active);
    filters.extend(sessions.iter().cloned().map(SessionFilter::Session));
    let current = filters
        .iter()
        .position(|filter| filter == current)
        .unwrap_or(0);
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

fn session_matches(
    session: &SessionState,
    allowed: &[String],
    filter: &SessionFilter,
    active: Option<&str>,
) -> bool {
    session.connected
        && (allowed.is_empty() || allowed.contains(&session.key))
        && match filter {
            SessionFilter::All => true,
            SessionFilter::Active => active == Some(session.key.as_str()),
            SessionFilter::Session(key) => key == &session.key,
        }
}

fn workspace_for_agent<'a>(
    session: &'a SessionState,
    agent: &AgentInfo,
) -> Option<&'a herdr_client::WorkspaceInfo> {
    session
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)
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

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Down => "down",
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
    }
}

fn action_name(action: &Action) -> &'static str {
    match action {
        Action::FocusSlot { .. } => "focus-slot",
        Action::FocusPane { .. } => "focus-pane",
        Action::SendKeys { .. } => "send-keys",
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
    use std::collections::HashMap;
    use std::io::Read;

    use herdr_client::WorkspaceInfo;

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

    fn agent(terminal_id: &str, pane_id: &str, focused: bool) -> AgentInfo {
        AgentInfo {
            terminal_id: terminal_id.into(),
            name: Some(terminal_id.into()),
            agent: Some("codex".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: None,
            agent_status: "working".into(),
            screen_detection_skipped: false,
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            agent_session: None,
            workspace_id: "workspace".into(),
            tab_id: "tab".into(),
            pane_id: pane_id.into(),
            focused,
            launch_pending: false,
            interactive_ready: true,
            state_change_seq: 1,
            cwd: None,
            foreground_cwd: None,
            revision: 0,
        }
    }

    fn session(key: &str, name: &str, connected: bool, focused: bool) -> SessionState {
        SessionState {
            key: key.into(),
            name: name.into(),
            connected,
            workspaces: vec![WorkspaceInfo {
                workspace_id: "workspace".into(),
                number: 1,
                label: name.into(),
                focused,
                pane_count: 1,
                tab_count: 1,
                active_tab_id: "tab".into(),
                agent_status: "working".into(),
                tokens: HashMap::new(),
                worktree: None,
            }],
            agents: vec![agent("terminal", "pane", focused)],
        }
    }

    fn test_client_route(endpoint: &str) -> frontends::ClientRoute {
        frontends::ClientRoute {
            client_id: "client-a".into(),
            socket_path: "/tmp/test-deck.sock".into(),
            endpoint_id: endpoint.into(),
            boot_id: Some("boot-a".into()),
        }
    }

    #[test]
    fn system_keys_create_commands_without_a_frontend_route() {
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
        let alpha = session("local", "alpha", true, false);
        let beta = session("ssh:beta", "beta", true, false);
        let stopped = session("ssh:stopped", "stopped", false, false);
        assert!(session_matches(&alpha, &[], &SessionFilter::All, None));
        assert!(!session_matches(&stopped, &[], &SessionFilter::All, None,));
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

        let mut vanished = SessionFilter::Session("ssh:beta".into());
        retain_filter(&mut vanished, &["local".into()]);
        assert_eq!(vanished, SessionFilter::All);
        let mut active_filter = SessionFilter::Active;
        retain_filter(&mut active_filter, &[]);
        assert_eq!(active_filter, SessionFilter::Active);
    }

    #[test]
    fn session_scoped_lookup_handles_duplicate_terminal_and_workspace_ids() {
        let alpha = session("local", "Alpha", true, true);
        let beta = session("ssh:beta", "Beta", true, true);
        assert_eq!(
            workspace_for_agent(&alpha, &alpha.agents[0]).unwrap().label,
            "Alpha"
        );
        assert_eq!(
            workspace_for_agent(&beta, &beta.agents[0]).unwrap().label,
            "Beta"
        );

        let remote = session("ssh:workbox", "Workbox", true, false);
        assert_eq!(session_label(&alpha), "Alpha");
        assert_eq!(session_label(&remote), "Workbox");
    }

    #[test]
    fn button_signature_tracks_session_indicators() {
        let plain = ButtonSignature {
            offline: false,
            terminal_id: Some("terminal".into()),
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

    #[test]
    fn queued_action_worker_rejects_latest_frontend_outage_without_sending() {
        let (commands, receiver) = mpsc::channel();
        let (events, results) = mpsc::channel();
        let route = ActionRoute {
            session: "local".into(),
            client_route: test_client_route("local"),
            source_pane: Some("pane".into()),
            target: crate::routing::AgentIdentity::from(&agent("terminal", "pane", false)),
            generation: 1,
        };
        commands
            .send(action_command("submit", route, Action::Submit))
            .unwrap();
        commands.send(ActionCommand::Stop).unwrap();
        action_worker(
            receiver,
            events,
            Arc::new(Mutex::new(RoutingState::default())),
        );
        let Event::Action { result, .. } = results.recv().unwrap() else {
            panic!("expected result")
        };
        assert_eq!(result.unwrap_err(), "Herdr frontend is unavailable");
    }

    #[test]
    fn actions_map_to_frontend_requests_with_captured_session() {
        let cases = [
            (
                Action::FocusSlot { slot: Some(0) },
                FrontendRequest::Navigate,
            ),
            (
                Action::FocusPane {
                    direction: Direction::Left,
                },
                FrontendRequest::Call {
                    method: "pane.focus_direction",
                    params: json!({ "pane_id": "pane-1", "direction": "left" }),
                },
            ),
            (
                Action::SendKeys {
                    keys: vec!["down".into()],
                },
                FrontendRequest::Input(Input::Keys(vec!["down".into()])),
            ),
            (
                Action::Prompt {
                    text: "review".into(),
                    submit: Some(false),
                },
                FrontendRequest::Input(Input::Text("review".into())),
            ),
            (
                Action::Prompt {
                    text: "review".into(),
                    submit: None,
                },
                FrontendRequest::Call {
                    method: "agent.prompt",
                    params: json!({ "target": "pane-1", "text": "review" }),
                },
            ),
            (
                Action::Submit,
                FrontendRequest::Input(Input::Keys(vec!["enter".into()])),
            ),
        ];

        for (action, expected) in cases {
            let ActionCommand::Call {
                name,
                route,
                request,
            } = action_command(
                "test",
                ActionRoute {
                    session: "local".into(),
                    client_route: test_client_route("local"),
                    source_pane: Some("source-pane".into()),
                    target: crate::routing::AgentIdentity::from(&agent(
                        "terminal", "pane-1", false,
                    )),
                    generation: 2,
                },
                action,
            )
            else {
                panic!("expected frontend call");
            };
            assert_eq!(name, "test");
            assert_eq!(route.session, "local");
            assert_eq!(route.source_pane.as_deref(), Some("source-pane"));
            assert_eq!(request, expected);
        }
    }
}

fn discovery_environment(directory: Option<&std::ffi::OsStr>) -> String {
    directory.filter(|p| !p.is_empty()).map(|p| format!(
        "<key>EnvironmentVariables</key><dict><key>HERDR_CLIENT_API_DIR</key><string>{}</string></dict>",
        escape_xml(&p.to_string_lossy())
    )).unwrap_or_default()
}

fn execute_frontend_call(
    client: &FrontendClient,
    route: &ActionRoute,
    request: &FrontendRequest,
    validate: &impl Fn() -> Result<(), String>,
) -> Result<serde_json::Value, String> {
    validate()?;
    let snapshot = client.snapshot().map_err(|e| e.to_string())?;
    crate::routing::validate_client(
        &frontends::ClientState {
            socket_path: client.socket_path().into(),
            snapshot,
        },
        route,
    )?;
    // Snapshot I/O can block while another TUI becomes focused or the source
    // changes. Recheck the global subscription guard after that I/O, without
    // holding its lock over the request. The server fences the endpoint route.
    validate()?;
    let wire = route.client_route.wire();
    match request {
        FrontendRequest::Navigate => {
            client.navigate(&wire, &NavigationTarget::Pane(route.target.pane_id.clone()))
        }
        FrontendRequest::Input(input) => client
            .input(input)
            .and_then(|accepted| serde_json::to_value(accepted).map_err(Into::into)),
        FrontendRequest::Call { method, params } => client.call(&wire, method, params.clone()),
    }
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod frontend_tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
    };

    #[test]
    fn discovery_override_is_preserved_and_xml_escaped() {
        assert_eq!(discovery_environment(None), "");
        assert_eq!(discovery_environment(Some(std::ffi::OsStr::new(""))), "");
        let env = discovery_environment(Some(std::ffi::OsStr::new("/tmp/a&b")));
        assert!(env.contains("HERDR_CLIENT_API_DIR"));
        assert!(env.contains("/tmp/a&amp;b"));
    }

    #[test]
    fn direct_navigation_input_and_calls_are_fenced_and_never_replayed() {
        for case in 0..8 {
            let path =
                std::env::temp_dir().join(format!("deck-wire-{}-{case}.sock", std::process::id()));
            let listener = UnixListener::bind(&path).unwrap();
            let mut observed = crate::routing::tests::client();
            observed.socket_path = path.clone();
            let mut routing = RoutingState::default();
            routing.update(vec![observed.clone()]);
            let route = routing.capture(&[], None, None).unwrap();
            let expected_route = route.client_route.wire();
            let server = thread::spawn(move || {
                let request = |reply: serde_json::Value| {
                    listener.set_nonblocking(true).unwrap();
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let stream = loop {
                        if let Ok((stream, _)) = listener.accept() {
                            break stream;
                        }
                        assert!(Instant::now() < deadline, "missing frontend request");
                        thread::sleep(Duration::from_millis(10));
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(stream);
                    writeln!(
                        reader.get_mut(),
                        "{}",
                        json!({"type":"hello","protocol":frontend::PROTOCOL,"client_id":"client-a"})
                    )
                    .unwrap();
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(value["protocol"], frontend::PROTOCOL);
                    if !reply.is_null() {
                        writeln!(reader.get_mut(), "{reply}").unwrap();
                    }
                    value
                };
                if case == 6 {
                    observed.snapshot.focused = Some(false);
                }
                assert_eq!(
                    request(json!({"type":"snapshot","id":1,"snapshot":observed.snapshot}))["type"],
                    "snapshot"
                );
                if case < 6 {
                    let reply = match case {
                        1 => {
                            json!({"type":"error","id":1,"error":{"code":"pane_not_found","message":"gone"}})
                        }
                        2 => {
                            json!({"type":"error","id":1,"error":{"code":"cancelled","message":"timeout"}})
                        }
                        3 => serde_json::Value::Null,
                        _ => json!({"type":"reply","id":1,"result":{"ok":true}}),
                    };
                    let value = request(reply);
                    match case {
                        4 => {
                            assert_eq!(value["type"], "call");
                            assert_eq!(value["route"], json!(expected_route));
                            assert_eq!(value["method"], "agent.prompt");
                            assert_eq!(value["params"], json!({"target":"p1","text":"hi"}));
                        }
                        5 => {
                            assert_eq!(value["type"], "input");
                            assert_eq!(value["keys"], json!(["enter"]));
                            assert!(value.get("route").is_none());
                        }
                        _ => {
                            assert_eq!(value["type"], "select");
                            assert_eq!(value["route"], json!(expected_route));
                            assert_eq!(value["target"], json!({"pane":"p1"}));
                        }
                    }
                }
                // Any retry becomes an extra queued connection, not another server response.
                thread::sleep(Duration::from_millis(50));
                assert!(listener.accept().is_err());
            });
            let request = match case {
                4 => FrontendRequest::Call {
                    method: "agent.prompt",
                    params: json!({"target":"p1","text":"hi"}),
                },
                5 => FrontendRequest::Input(Input::Keys(vec!["enter".into()])),
                _ => FrontendRequest::Navigate,
            };
            let result = execute_frontend_call(
                &FrontendClient::connect(&path).with_timeout(Duration::from_secs(2)),
                &route,
                &request,
                &{
                    let checks = std::cell::Cell::new(0);
                    move || {
                        checks.set(checks.get() + 1);
                        if case == 7 && checks.get() == 2 {
                            Err("stale during snapshot".into())
                        } else {
                            Ok(())
                        }
                    }
                },
            );
            assert_eq!(
                result.is_ok(),
                matches!(case, 0 | 4 | 5),
                "case {case}: {result:?}"
            );
            server.join().unwrap();
            fs::remove_file(path).unwrap();
        }
    }
}
