//! Per-user Codex Micro device service and typed client.

use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use signal_hook::consts::SIGTERM;

use crate::{
    DeviceEvent, DeviceInfo, ExternalOwner, InputMonitoringAccess,
    device::{MicroDevice, input_monitoring_access, input_monitoring_request_access},
    external_owner,
    keymap::{MAX_KEYMAP_SIZE, read_keymap_until, write_keymap_until},
};

const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME_BYTES: usize = 12 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const CALL_TIMEOUT: Duration = Duration::from_secs(6);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_OPERATION_TIMEOUT: Duration = Duration::from_secs(15);
const ACQUIRE_CLIENT_TIMEOUT: Duration = Duration::from_secs(17);
const KEYMAP_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const KEYMAP_CLIENT_TIMEOUT: Duration = Duration::from_secs(35);
const HANDY_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_DELAY: Duration = Duration::from_secs(1);
const OWNER_POLL_INTERVAL: Duration = Duration::from_millis(250);
const EVENT_QUEUE_SIZE: usize = 256;
const SLOT_COUNT: usize = 6;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FocusedApp {
    #[serde(rename = "appName")]
    pub app_name: String,
    pub process: String,
}

/// The linked-app identity that Micro setup writes for a device layer. Layer 1
/// is the native layer the service restores after a controller disconnect.
pub fn layer_identity(layer: usize) -> FocusedApp {
    FocusedApp {
        app_name: format!("Herdr Micro Layer {layer}"),
        process: format!("gjermundgaraba.herdr-micro.layer-{layer}"),
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Light {
    pub c: u32,
    pub b: f64,
    pub e: u8,
    pub s: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lighting {
    pub aggregate: BTreeMap<String, Light>,
    pub slots: Vec<Light>,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            aggregate: BTreeMap::new(),
            slots: vec![Light::default(); SLOT_COUNT],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServiceStatus {
    pub device: Option<DeviceInfo>,
    pub external_owner: Option<ExternalOwner>,
    pub last_error: Option<String>,
    pub input_monitoring: InputMonitoringAccess,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "camelCase")]
enum Operation {
    Status,
    Authorize,
    Acquire,
    SetFocusedApp { focused_app: FocusedApp },
    SetLighting { lighting: Lighting },
    ReadKeymap,
    WriteKeymap { data: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "camelCase")]
enum ClientMessage {
    Hello {
        protocol: u32,
    },
    Call {
        protocol: u32,
        id: u64,
        operation: Operation,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "camelCase")]
enum ServiceResult {
    Unit,
    Status { status: ServiceStatus },
    Device { device: DeviceInfo },
    Keymap { data: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "camelCase")]
enum ServerMessage {
    Hello {
        protocol: u32,
    },
    Reply {
        protocol: u32,
        id: u64,
        result: Option<ServiceResult>,
        error: Option<String>,
    },
    Event {
        protocol: u32,
        event: DeviceEvent,
    },
    Error {
        protocol: u32,
        error: String,
    },
}

type Reply = std::result::Result<ServiceResult, String>;
type PendingReply = Arc<Mutex<Option<(u64, SyncSender<Reply>)>>>;

pub struct Client {
    writer: UnixStream,
    pending: PendingReply,
    next_id: Cell<u64>,
    closed: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    event_tx: Sender<DeviceEvent>,
    reader: Option<JoinHandle<()>>,
}

impl Client {
    /// Whether the service connection has ended. A physical-device outage does
    /// not close this connection; the service owns reconnecting the hardware.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn connect(event_tx: Sender<DeviceEvent>) -> Result<Self> {
        Self::connect_at(&socket_path(), event_tx)
    }

    fn connect_at(path: &Path, event_tx: Sender<DeviceEvent>) -> Result<Self> {
        let mut stream = UnixStream::connect(path)
            .with_context(|| format!("connect Codex Micro service at {}", path.display()))?;
        require_peer_uid(&stream, effective_uid())?;
        stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
        stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
        write_message(
            &mut stream,
            &ClientMessage::Hello {
                protocol: PROTOCOL_VERSION,
            },
        )?;
        let mut reader = BufReader::new(stream.try_clone()?);
        match read_message::<ServerMessage>(&mut reader)? {
            ServerMessage::Hello { protocol } if protocol == PROTOCOL_VERSION => {}
            ServerMessage::Error { protocol, error } if protocol == PROTOCOL_VERSION => {
                bail!(error)
            }
            _ => bail!("incompatible Codex Micro service protocol"),
        }
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(Some(CALL_TIMEOUT))?;

        let pending = Arc::new(Mutex::new(None));
        let closed = Arc::new(AtomicBool::new(false));
        let closing = Arc::new(AtomicBool::new(false));
        let reader_pending = Arc::clone(&pending);
        let reader_closed = Arc::clone(&closed);
        let reader_closing = Arc::clone(&closing);
        let reader = thread::Builder::new()
            .name("codex-micro-service-client".into())
            .spawn({
                let event_tx = event_tx.clone();
                move || {
                    client_reader(
                        reader,
                        event_tx,
                        reader_pending,
                        reader_closed,
                        reader_closing,
                    )
                }
            })?;
        Ok(Self {
            writer: stream,
            pending,
            next_id: Cell::new(1),
            closed,
            closing,
            event_tx,
            reader: Some(reader),
        })
    }

    pub fn status(&self) -> Result<ServiceStatus> {
        match self.call(Operation::Status, CALL_TIMEOUT)? {
            ServiceResult::Status { status } => Ok(status),
            _ => bail!("invalid status response"),
        }
    }

    pub fn authorize(&self) -> Result<InputMonitoringAccess> {
        match self.call(Operation::Authorize, CALL_TIMEOUT)? {
            ServiceResult::Status { status } => Ok(status.input_monitoring),
            _ => bail!("invalid authorization response"),
        }
    }

    pub fn acquire(&self) -> Result<DeviceInfo> {
        match self.call(Operation::Acquire, ACQUIRE_CLIENT_TIMEOUT)? {
            ServiceResult::Device { device } => Ok(device),
            _ => bail!("invalid acquire response"),
        }
    }

    pub fn set_focused_app(&self, focused_app: FocusedApp) -> Result<()> {
        expect_unit(self.call(Operation::SetFocusedApp { focused_app }, CALL_TIMEOUT)?)
    }

    pub fn set_lighting(&self, lighting: Lighting) -> Result<()> {
        expect_unit(self.call(Operation::SetLighting { lighting }, CALL_TIMEOUT)?)
    }

    pub fn read_keymap(&self) -> Result<Vec<u8>> {
        match self.call(Operation::ReadKeymap, KEYMAP_CLIENT_TIMEOUT)? {
            ServiceResult::Keymap { data } => {
                if data.len() > encoded_keymap_limit() {
                    bail!("keymap exceeds {MAX_KEYMAP_SIZE} bytes")
                }
                let bytes = STANDARD
                    .decode(data)
                    .context("invalid keymap data from service")?;
                if bytes.len() > MAX_KEYMAP_SIZE {
                    bail!("keymap exceeds {MAX_KEYMAP_SIZE} bytes")
                }
                Ok(bytes)
            }
            _ => bail!("invalid keymap response"),
        }
    }

    pub fn write_keymap(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_KEYMAP_SIZE {
            bail!("keymap exceeds {MAX_KEYMAP_SIZE} bytes")
        }
        expect_unit(self.call(
            Operation::WriteKeymap {
                data: STANDARD.encode(bytes),
            },
            KEYMAP_CLIENT_TIMEOUT,
        )?)
    }

    fn call(&self, operation: Operation, timeout: Duration) -> Result<ServiceResult> {
        if self.closed.load(Ordering::Acquire) {
            bail!("Codex Micro service disconnected")
        }
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| anyhow!("Codex Micro service reply slot poisoned"))?;
        if pending.is_some() {
            bail!("another Codex Micro service call is in progress")
        }
        *pending = Some((id, reply_tx));
        drop(pending);
        let message = ClientMessage::Call {
            protocol: PROTOCOL_VERSION,
            id,
            operation,
        };
        if let Err(error) = write_message(&mut &self.writer, &message) {
            self.remove_pending(id);
            self.disconnect(error.to_string());
            return Err(error);
        }
        match reply_rx.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(anyhow!(error)),
            Err(_) => {
                self.remove_pending(id);
                bail!("Codex Micro service request {id} timed out")
            }
        }
    }

    fn remove_pending(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock()
            && pending
                .as_ref()
                .is_some_and(|(pending_id, _)| *pending_id == id)
        {
            pending.take();
        }
    }

    fn disconnect(&self, error: String) {
        let notify =
            !self.closing.load(Ordering::Acquire) && !self.closed.swap(true, Ordering::AcqRel);
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
        if notify {
            let _ = self.event_tx.send(DeviceEvent::Disconnected { error });
        }
    }

    pub fn close(&mut self) -> Result<()> {
        self.closing.store(true, Ordering::Release);
        self.closed.store(true, Ordering::Release);
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
        match self.reader.take().map(JoinHandle::join) {
            Some(Err(_)) => Err(anyhow!("Codex Micro service reader panicked")),
            _ => Ok(()),
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn expect_unit(result: ServiceResult) -> Result<()> {
    match result {
        ServiceResult::Unit => Ok(()),
        _ => bail!("invalid Codex Micro service response"),
    }
}

fn client_reader(
    mut reader: BufReader<UnixStream>,
    event_tx: Sender<DeviceEvent>,
    pending: PendingReply,
    closed: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
) {
    let result = (|| -> Result<()> {
        loop {
            match read_message::<ServerMessage>(&mut reader)? {
                ServerMessage::Reply {
                    protocol,
                    id,
                    result,
                    error,
                } if protocol == PROTOCOL_VERSION => {
                    let reply = match (result, error) {
                        (Some(result), None) => Ok(result),
                        (None, Some(error)) => Err(error),
                        _ => bail!("invalid Codex Micro service reply"),
                    };
                    if let Some(tx) = pending.lock().ok().and_then(|mut pending| {
                        pending
                            .as_ref()
                            .is_some_and(|(pending_id, _)| *pending_id == id)
                            .then(|| pending.take().unwrap().1)
                    }) {
                        let _ = tx.send(reply);
                    }
                }
                ServerMessage::Event { protocol, event } if protocol == PROTOCOL_VERSION => {
                    let _ = event_tx.send(event);
                }
                ServerMessage::Error { protocol, error } if protocol == PROTOCOL_VERSION => {
                    bail!(error)
                }
                _ => bail!("incompatible Codex Micro service protocol"),
            }
        }
    })();
    let was_closed = closed.swap(true, Ordering::AcqRel);
    let intentional = closing.load(Ordering::Acquire) || was_closed;
    let error = result
        .err()
        .map(|error| error.to_string())
        .unwrap_or_else(|| "Codex Micro service disconnected".into());
    if let Ok(mut pending) = pending.lock()
        && let Some((_, reply)) = pending.take()
    {
        let _ = reply.send(Err(error.clone()));
    }
    if !intentional {
        let _ = event_tx.send(DeviceEvent::Disconnected { error });
    }
}

enum ServiceCommand {
    Connected {
        client: u64,
        events: SyncSender<ServerMessage>,
        shutdown: UnixStream,
    },
    Disconnected {
        client: u64,
    },
    Call {
        client: u64,
        operation: Operation,
        deadline: Instant,
        reply: SyncSender<Reply>,
    },
}

struct ClientConnection {
    events: SyncSender<ServerMessage>,
    shutdown: UnixStream,
}

struct ServiceState {
    clients: HashMap<u64, ClientConnection>,
    controller: Option<u64>,
    external_owner: Option<ExternalOwner>,
    device: Option<MicroDevice>,
    device_info: Option<DeviceInfo>,
    last_error: Option<String>,
    focused_app: Option<FocusedApp>,
    lighting: Option<Lighting>,
    next_retry: Instant,
    device_event_tx: Sender<DeviceEvent>,
    handy_tx: Sender<()>,
}

impl ServiceState {
    fn new(
        device_event_tx: Sender<DeviceEvent>,
        handy_tx: Sender<()>,
        external_owner: Option<ExternalOwner>,
    ) -> Self {
        Self {
            clients: HashMap::new(),
            controller: None,
            external_owner,
            device: None,
            device_info: None,
            last_error: None,
            focused_app: None,
            lighting: None,
            next_retry: Instant::now(),
            device_event_tx,
            handy_tx,
        }
    }

    fn status(&self) -> ServiceStatus {
        ServiceStatus {
            device: self.device_info.clone(),
            external_owner: self.external_owner,
            last_error: self.last_error.clone(),
            input_monitoring: input_monitoring_access(),
        }
    }

    fn handle(&mut self, command: ServiceCommand) {
        match command {
            ServiceCommand::Connected {
                client,
                events,
                shutdown,
            } => {
                self.clients
                    .insert(client, ClientConnection { events, shutdown });
            }
            ServiceCommand::Disconnected { client } => {
                self.disconnect_client(client, false);
            }
            ServiceCommand::Call {
                client,
                operation,
                deadline,
                reply,
            } => {
                let _ = reply.send(
                    self.call(client, operation, deadline)
                        .map_err(|error| error.to_string()),
                );
            }
        }
    }

    fn call(
        &mut self,
        client: u64,
        operation: Operation,
        deadline: Instant,
    ) -> Result<ServiceResult> {
        check_deadline(deadline)?;
        if !self.clients.contains_key(&client) {
            bail!("Codex Micro service client disconnected")
        }
        match operation {
            Operation::Status => Ok(ServiceResult::Status {
                status: self.status(),
            }),
            Operation::Authorize => {
                input_monitoring_request_access();
                self.next_retry = Instant::now();
                Ok(ServiceResult::Status {
                    status: self.status(),
                })
            }
            Operation::Acquire => {
                if self.controller.is_some_and(|owner| owner != client) {
                    bail!("another client controls the Codex Micro")
                }
                self.refresh_external_owner(external_owner());
                if let Some(owner) = self.external_owner {
                    bail!("Codex Micro is owned by {owner}")
                }
                let device = match self.device_info.clone() {
                    Some(device) => device,
                    None => self.open_device(deadline)?,
                };
                self.controller = Some(client);
                Ok(ServiceResult::Device { device })
            }
            Operation::SetFocusedApp { focused_app } => {
                self.require_controller(client)?;
                validate_focused_app(&focused_app)?;
                self.apply_focused_app(&focused_app, deadline)?;
                self.focused_app = Some(focused_app);
                Ok(ServiceResult::Unit)
            }
            Operation::SetLighting { lighting } => {
                self.require_controller(client)?;
                validate_lighting(&lighting)?;
                self.apply_lighting(&lighting, deadline)?;
                self.lighting = Some(lighting);
                Ok(ServiceResult::Unit)
            }
            Operation::ReadKeymap => {
                self.require_controller(client)?;
                let result = {
                    let device = self
                        .device
                        .as_ref()
                        .ok_or_else(|| anyhow!("Codex Micro is unavailable"))?;
                    read_keymap_until(
                        |method, params| {
                            ensure_no_external_owner()?;
                            device.request(method, params)
                        },
                        deadline,
                    )
                };
                self.refresh_external_owner(external_owner());
                let bytes = result?;
                check_deadline(deadline)?;
                Ok(ServiceResult::Keymap {
                    data: STANDARD.encode(bytes),
                })
            }
            Operation::WriteKeymap { data } => {
                self.require_controller(client)?;
                if data.len() > encoded_keymap_limit() {
                    bail!("keymap exceeds {MAX_KEYMAP_SIZE} bytes")
                }
                let bytes = STANDARD.decode(data).context("invalid keymap data")?;
                if bytes.is_empty() {
                    bail!("keymap is empty")
                }
                if bytes.len() > MAX_KEYMAP_SIZE {
                    bail!("keymap exceeds {MAX_KEYMAP_SIZE} bytes")
                }
                let result = {
                    let device = self
                        .device
                        .as_ref()
                        .ok_or_else(|| anyhow!("Codex Micro is unavailable"))?;
                    write_keymap_until(
                        |method, params| {
                            ensure_no_external_owner()?;
                            device.request(method, params)
                        },
                        &bytes,
                        deadline,
                    )
                };
                self.refresh_external_owner(external_owner());
                result?;
                check_deadline(deadline)?;
                Ok(ServiceResult::Unit)
            }
        }
    }

    fn require_controller(&self, client: u64) -> Result<()> {
        if self.controller == Some(client) {
            Ok(())
        } else {
            bail!("client does not control the Codex Micro")
        }
    }

    fn disconnect_client(&mut self, client: u64, force_socket: bool) {
        if let Some(connection) = self.clients.remove(&client)
            && force_socket
        {
            let _ = connection.shutdown.shutdown(std::net::Shutdown::Both);
        }
        self.cleanup_controller(client);
    }

    fn cleanup_controller(&mut self, client: u64) {
        if self.controller != Some(client) {
            return;
        }
        self.controller = None;
        // Replay the pre-Herdr state on the next open — native layer, blank
        // lights — so a dead controller cannot strand Layer 2 status lighting.
        // A graceful stop sends exactly this state before disconnecting.
        // Absent aggregate zones stay unchanged on the device, so every
        // previously lit zone needs an explicit default entry.
        self.focused_app = Some(layer_identity(1));
        self.lighting = Some(Lighting {
            aggregate: self
                .lighting
                .take()
                .map(|lighting| {
                    lighting
                        .aggregate
                        .into_keys()
                        .map(|zone| (zone, Light::default()))
                        .collect()
                })
                .unwrap_or_default(),
            ..Lighting::default()
        });
        if let Err(error) = self.close_device() {
            self.last_error = Some(error.to_string());
        }
        self.next_retry = Instant::now();
    }

    fn open_device(&mut self, deadline: Instant) -> Result<DeviceInfo> {
        check_deadline(deadline)?;
        self.refresh_external_owner(external_owner());
        if let Some(owner) = self.external_owner {
            bail!("Codex Micro is owned by {owner}")
        }
        let permission_error = match input_monitoring_access() {
            InputMonitoringAccess::Granted => None,
            InputMonitoringAccess::Denied => Some("Input Monitoring access is denied"),
            InputMonitoringAccess::Unknown => Some("Input Monitoring access is required"),
        };
        if let Some(error) = permission_error {
            self.last_error = Some(error.into());
            self.next_retry = Instant::now() + RETRY_DELAY;
            bail!(error)
        }
        let (mut device, info) = match MicroDevice::open(self.device_event_tx.clone()) {
            Ok(opened) => opened,
            Err(error) => {
                self.last_error = Some(error.to_string());
                self.next_retry = Instant::now() + RETRY_DELAY;
                return Err(error);
            }
        };
        if let Some(owner) = external_owner() {
            let _ = device.close();
            self.external_owner = Some(owner);
            self.next_retry = Instant::now() + RETRY_DELAY;
            bail!("Codex Micro is owned by {owner}")
        }
        if let Err(error) = replay(
            &device,
            self.focused_app.as_ref(),
            self.lighting.as_ref(),
            deadline,
        ) {
            let _ = device.close();
            self.external_owner = external_owner();
            self.last_error = Some(error.to_string());
            self.next_retry = Instant::now() + RETRY_DELAY;
            return Err(error);
        }
        self.device = Some(device);
        self.device_info = Some(info.clone());
        self.last_error = None;
        Ok(info)
    }

    fn apply_focused_app(&mut self, focused_app: &FocusedApp, deadline: Instant) -> Result<()> {
        let Some(device) = self.device.as_ref() else {
            return Ok(());
        };
        if let Err(error) = send_focused_app(device, focused_app, deadline) {
            if let Some(owner) = external_owner() {
                self.refresh_external_owner(Some(owner));
                return Err(error);
            }
            self.device_failed(error.to_string());
            return Err(error);
        }
        Ok(())
    }

    fn apply_lighting(&mut self, lighting: &Lighting, deadline: Instant) -> Result<()> {
        let Some(device) = self.device.as_ref() else {
            return Ok(());
        };
        if let Err(error) = send_lighting(device, lighting, deadline) {
            if let Some(owner) = external_owner() {
                self.refresh_external_owner(Some(owner));
                return Err(error);
            }
            self.device_failed(error.to_string());
            return Err(error);
        }
        Ok(())
    }

    fn device_failed(&mut self, error: String) {
        self.last_error = Some(error);
        self.device_info = None;
        if let Some(mut device) = self.device.take() {
            let _ = device.close();
        }
        self.next_retry = Instant::now() + RETRY_DELAY;
    }

    fn close_device(&mut self) -> Result<()> {
        self.device_info = None;
        match self.device.take() {
            Some(mut device) => device.close(),
            None => Ok(()),
        }
    }

    fn refresh_external_owner(&mut self, owner: Option<ExternalOwner>) {
        if self.external_owner == owner {
            return;
        }
        self.external_owner = owner;
        if owner.is_some() {
            if let Err(error) = self.close_device() {
                self.last_error = Some(error.to_string());
            }
        } else {
            self.next_retry = Instant::now();
        }
    }

    fn device_event(&mut self, event: DeviceEvent) {
        self.device_event_with_owner(event, external_owner());
    }

    fn device_event_with_owner(
        &mut self,
        event: DeviceEvent,
        external_owner: Option<ExternalOwner>,
    ) {
        self.refresh_external_owner(external_owner);
        if let DeviceEvent::Disconnected { error } = &event {
            self.device_failed(error.clone());
        }
        if self.external_owner.is_some() {
            return;
        }
        if let Some(pressed) = handy_event(&event) {
            if pressed && self.handy_tx.send(()).is_err() {
                eprintln!("Codex Micro Handy worker stopped");
            }
            return;
        }
        let Some(controller) = self.controller else {
            return;
        };
        let Some(connection) = self.clients.get(&controller) else {
            self.cleanup_controller(controller);
            return;
        };
        let message = ServerMessage::Event {
            protocol: PROTOCOL_VERSION,
            event,
        };
        match connection.events.try_send(message) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.disconnect_client(controller, true);
            }
        }
    }

    fn retry(&mut self) {
        if self.external_owner.is_none()
            && self.device.is_none()
            && Instant::now() >= self.next_retry
        {
            let _ = self.open_device(Instant::now() + ACQUIRE_OPERATION_TIMEOUT);
        }
    }
}

fn validate_focused_app(app: &FocusedApp) -> Result<()> {
    if app.app_name.trim().is_empty()
        || app.process.trim().is_empty()
        || app.app_name.len() > 256
        || app.process.len() > 256
    {
        bail!("focused app name and process must be nonempty")
    }
    Ok(())
}

fn validate_lighting(lighting: &Lighting) -> Result<()> {
    if lighting.aggregate.len() > 64 || lighting.slots.len() != SLOT_COUNT {
        bail!("lighting must contain six slots and at most 64 aggregate zones")
    }
    for zone in lighting.aggregate.keys() {
        if zone.is_empty() || zone.len() > 128 {
            bail!("invalid lighting zone")
        }
    }
    for light in lighting.aggregate.values().chain(lighting.slots.iter()) {
        if light.c > 0xFF_FFFF
            || !light.b.is_finite()
            || !(0.0..=1.0).contains(&light.b)
            || light.e > 6
            || !light.s.is_finite()
            || !(0.0..=1.0).contains(&light.s)
        {
            bail!("invalid light")
        }
    }
    Ok(())
}

fn replay(
    device: &MicroDevice,
    focused_app: Option<&FocusedApp>,
    lighting: Option<&Lighting>,
    deadline: Instant,
) -> Result<()> {
    check_deadline(deadline)?;
    ensure_no_external_owner()?;
    if let Some(focused_app) = focused_app {
        send_focused_app(device, focused_app, deadline)?;
    }
    if let Some(lighting) = lighting {
        send_lighting(device, lighting, deadline)?;
    }
    check_deadline(deadline)
}

fn send_focused_app(
    device: &MicroDevice,
    focused_app: &FocusedApp,
    deadline: Instant,
) -> Result<()> {
    check_deadline(deadline)?;
    ensure_no_external_owner()?;
    device.send("host.focused_app", Some(serde_json::to_value(focused_app)?))?;
    ensure_no_external_owner()?;
    check_deadline(deadline)
}

fn send_lighting(device: &MicroDevice, lighting: &Lighting, deadline: Instant) -> Result<()> {
    check_deadline(deadline)?;
    ensure_no_external_owner()?;
    device.send(
        "v.oai.rgbcfg",
        Some(serde_json::to_value(&lighting.aggregate)?),
    )?;
    ensure_no_external_owner()?;
    check_deadline(deadline)?;
    ensure_no_external_owner()?;
    let slots: Vec<Value> = lighting
        .slots
        .iter()
        .enumerate()
        .map(|(id, light)| json!({"id":id,"c":light.c,"b":light.b,"e":light.e,"s":light.s}))
        .collect();
    device.send("v.oai.thstatus", Some(Value::Array(slots)))?;
    ensure_no_external_owner()?;
    check_deadline(deadline)
}

fn check_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        bail!("Codex Micro service operation timed out")
    }
    Ok(())
}

fn ensure_no_external_owner() -> Result<()> {
    if let Some(owner) = external_owner() {
        bail!("Codex Micro is owned by {owner}")
    }
    Ok(())
}

fn handy_event(event: &DeviceEvent) -> Option<bool> {
    match event {
        DeviceEvent::Key { key, action } if key == "ACT10" => Some(*action == 1),
        _ => None,
    }
}

fn socket_path() -> PathBuf {
    PathBuf::from(format!("/tmp/dev.codex-micro-{}.sock", effective_uid()))
}

pub fn run() -> Result<()> {
    let uid = effective_uid();
    if uid == 0 {
        bail!("Codex Micro service refuses to run as root")
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGTERM, Arc::clone(&shutdown))
        .context("install SIGTERM handler")?;
    let _service_lock = ServiceLock::acquire(&service_lock_path(uid))?;
    let path = socket_path();
    let (listener, _cleanup) = bind_socket(&path)?;
    run_listener(listener, uid, &shutdown)
}

fn run_listener(listener: UnixListener, uid: libc::uid_t, shutdown: &AtomicBool) -> Result<()> {
    listener.set_nonblocking(true)?;
    let next_client = AtomicU64::new(1);
    let (service_tx, service_rx) = mpsc::channel();
    let (device_event_tx, device_event_rx) = mpsc::channel();
    let (handy_tx, handy_shutdown, handy_worker) = spawn_handy_worker()?;
    let mut state = ServiceState::new(device_event_tx, handy_tx, external_owner());
    let mut next_owner_poll = Instant::now() + OWNER_POLL_INTERVAL;
    while !shutdown.load(Ordering::Acquire) {
        if Instant::now() >= next_owner_poll {
            state.refresh_external_owner(external_owner());
            next_owner_poll = Instant::now() + OWNER_POLL_INTERVAL;
        }
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    if require_peer_uid(&stream, uid).is_err() {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        continue;
                    }
                    stream.set_nonblocking(false)?;
                    let client = next_client.fetch_add(1, Ordering::Relaxed);
                    let service_tx = service_tx.clone();
                    thread::Builder::new()
                        .name("codex-micro-service-connection".into())
                        .spawn(move || {
                            let _ = serve_connection(stream, client, service_tx);
                        })?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("accept Codex Micro service client"),
            }
        }
        while let Ok(command) = service_rx.try_recv() {
            handle_command_after_events(&mut state, command, &device_event_rx, external_owner);
        }
        while let Ok(event) = device_event_rx.try_recv() {
            state.device_event(event);
        }
        state.retry();
        thread::sleep(Duration::from_millis(10));
    }
    state.controller = None;
    let close = state.close_device();
    handy_shutdown.store(true, Ordering::Release);
    drop(state);
    let worker = handy_worker
        .join()
        .map_err(|_| anyhow!("Codex Micro Handy worker panicked"));
    close.and(worker)
}

fn handle_command_after_events(
    state: &mut ServiceState,
    command: ServiceCommand,
    device_events: &Receiver<DeviceEvent>,
    detect_owner: impl Fn() -> Option<ExternalOwner>,
) {
    while let Ok(event) = device_events.try_recv() {
        state.device_event_with_owner(event, detect_owner());
    }
    state.handle(command);
}

fn spawn_handy_worker() -> Result<(Sender<()>, Arc<AtomicBool>, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let worker_shutdown = Arc::clone(&shutdown);
    let worker = thread::Builder::new()
        .name("codex-micro-handy".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                if worker_shutdown.load(Ordering::Acquire) {
                    break;
                }
                if let Err(error) = toggle_handy() {
                    eprintln!("Codex Micro Handy action failed: {error:#}");
                }
            }
        })?;
    Ok((tx, shutdown, worker))
}

fn toggle_handy() -> Result<()> {
    let mut child = Command::new("/Applications/Handy.app/Contents/MacOS/handy")
        .arg("--toggle-transcription")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start Handy")?;
    let deadline = Instant::now() + HANDY_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().context("wait for Handy")? {
            if status.success() {
                return Ok(());
            }
            bail!("Handy exited with {status}")
        }
        if Instant::now() >= deadline {
            let kill = child.kill();
            child.wait().context("reap timed-out Handy")?;
            kill.context("stop timed-out Handy")?;
            bail!("Handy timed out after {} seconds", HANDY_TIMEOUT.as_secs())
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn serve_connection(
    mut stream: UnixStream,
    client: u64,
    service_tx: Sender<ServiceCommand>,
) -> Result<()> {
    stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    match read_message::<ClientMessage>(&mut reader) {
        Ok(ClientMessage::Hello { protocol }) if protocol == PROTOCOL_VERSION => {}
        Ok(_) => {
            let _ = write_message(
                &mut stream,
                &ServerMessage::Error {
                    protocol: PROTOCOL_VERSION,
                    error: "incompatible Codex Micro service protocol".into(),
                },
            );
            bail!("incompatible Codex Micro service protocol")
        }
        Err(error) => {
            let _ = write_message(
                &mut stream,
                &ServerMessage::Error {
                    protocol: PROTOCOL_VERSION,
                    error: error.to_string(),
                },
            );
            return Err(error);
        }
    }
    write_message(
        &mut stream,
        &ServerMessage::Hello {
            protocol: PROTOCOL_VERSION,
        },
    )?;
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(CALL_TIMEOUT))?;

    let (outgoing_tx, outgoing_rx) = mpsc::sync_channel(EVENT_QUEUE_SIZE);
    let mut outgoing_stream = stream.try_clone()?;
    thread::Builder::new()
        .name("codex-micro-service-writer".into())
        .spawn(move || connection_writer(&mut outgoing_stream, outgoing_rx))?;
    service_tx.send(ServiceCommand::Connected {
        client,
        events: outgoing_tx.clone(),
        shutdown: stream.try_clone()?,
    })?;

    let result = (|| -> Result<()> {
        loop {
            let (id, operation) = match read_message::<ClientMessage>(&mut reader)? {
                ClientMessage::Call {
                    protocol,
                    id,
                    operation,
                } if protocol == PROTOCOL_VERSION && id != 0 => (id, operation),
                _ => bail!("incompatible Codex Micro service protocol"),
            };
            let deadline = Instant::now() + operation_timeout(&operation);
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            service_tx.send(ServiceCommand::Call {
                client,
                operation,
                deadline,
                reply: reply_tx,
            })?;
            let reply = reply_rx
                .recv()
                .map_err(|_| anyhow!("Codex Micro service stopped"))?;
            let (result, error) = match reply {
                Ok(result) => (Some(result), None),
                Err(error) => (None, Some(error)),
            };
            outgoing_tx
                .send(ServerMessage::Reply {
                    protocol: PROTOCOL_VERSION,
                    id,
                    result,
                    error,
                })
                .map_err(|_| anyhow!("Codex Micro service writer stopped"))?;
        }
    })();
    let _ = service_tx.send(ServiceCommand::Disconnected { client });
    drop(outgoing_tx);
    result
}

fn connection_writer(stream: &mut UnixStream, outgoing: Receiver<ServerMessage>) {
    while let Ok(message) = outgoing.recv() {
        if write_message(stream, &message).is_err() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
    }
}

fn read_message<T: DeserializeOwned>(reader: &mut impl BufRead) -> Result<T> {
    let line = read_line(reader)?;
    serde_json::from_slice(&line).context("parse Codex Micro service message")
}

fn read_line(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    reader
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if line.last() == Some(&b'\n') {
        if line.len() > MAX_FRAME_BYTES {
            bail!("Codex Micro service message exceeds {MAX_FRAME_BYTES} bytes")
        }
        line.pop();
        return Ok(line);
    }
    if line.len() > MAX_FRAME_BYTES {
        bail!("Codex Micro service message exceeds {MAX_FRAME_BYTES} bytes")
    }
    bail!("Codex Micro service connection closed")
}

fn write_message(stream: &mut impl Write, message: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec(message)?;
    if bytes.len() + 1 > MAX_FRAME_BYTES {
        bail!("Codex Micro service message exceeds {MAX_FRAME_BYTES} bytes")
    }
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    Ok(())
}

fn encoded_keymap_limit() -> usize {
    MAX_KEYMAP_SIZE.div_ceil(3) * 4
}

fn operation_timeout(operation: &Operation) -> Duration {
    match operation {
        Operation::Acquire => ACQUIRE_OPERATION_TIMEOUT,
        Operation::ReadKeymap | Operation::WriteKeymap { .. } => KEYMAP_OPERATION_TIMEOUT,
        _ => OPERATION_TIMEOUT,
    }
}

fn effective_uid() -> libc::uid_t {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

fn service_lock_path(uid: libc::uid_t) -> PathBuf {
    PathBuf::from(format!("/tmp/dev.codex-micro-{uid}.service.lock"))
}

/// Dropping the owned file closes its descriptor, which releases the lock.
struct ServiceLock(#[allow(dead_code)] File);

impl ServiceLock {
    fn acquire(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .with_context(|| format!("open service lock {}", path.display()))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != effective_uid()
            || metadata.nlink() != 1
            || metadata.mode() & 0o7777 != 0o600
        {
            bail!("unsafe Codex Micro service lock {}", path.display())
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                bail!("Codex Micro service is already running")
            }
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("lock service {}", path.display()));
            }
        }
        Ok(Self(file))
    }
}

fn require_peer_uid(stream: &UnixStream, expected: libc::uid_t) -> Result<()> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: both output pointers are valid and the stream owns a live descriptor.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error())
            .context("read Codex Micro service peer credentials");
    }
    if uid != expected {
        bail!("Codex Micro service peer has unexpected uid {uid}")
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

fn socket_identity(path: &Path) -> Result<Option<SocketIdentity>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() || metadata.uid() != effective_uid() {
                bail!(
                    "refusing unsafe Codex Micro service socket {}",
                    path.display()
                )
            }
            Ok(Some(SocketIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            }))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

struct SocketCleanup {
    path: PathBuf,
    identity: SocketIdentity,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        if socket_identity(&self.path).ok().flatten() == Some(self.identity) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn bind_socket(path: &Path) -> Result<(UnixListener, SocketCleanup)> {
    let first = bind_private(path);
    let listener = match first {
        Ok(listener) => listener,
        Err(error) if error.raw_os_error() == Some(libc::EADDRINUSE) => {
            let before = socket_identity(path)?;
            match UnixStream::connect(path) {
                Ok(_) => bail!("Codex Micro service is already running"),
                Err(error) if error.raw_os_error() == Some(libc::ECONNREFUSED) => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("connect {}", path.display()));
                }
            }
            if before.is_none() || socket_identity(path)? != before {
                bail!("Codex Micro service socket changed during startup")
            }
            fs::remove_file(path).with_context(|| format!("remove stale {}", path.display()))?;
            bind_private(path).with_context(|| format!("bind {}", path.display()))?
        }
        Err(error) => return Err(error).with_context(|| format!("bind {}", path.display())),
    };
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod {}", path.display()))?;
    let identity = socket_identity(path)?.ok_or_else(|| anyhow!("service socket disappeared"))?;
    Ok((
        listener,
        SocketCleanup {
            path: path.to_owned(),
            identity,
        },
    ))
}

fn bind_private(path: &Path) -> io::Result<UnixListener> {
    // SAFETY: umask has no preconditions; service startup is single-threaded here.
    let previous_umask = unsafe { libc::umask(0o077) };
    let result = UnixListener::bind(path);
    // SAFETY: restores the process umask immediately after bind.
    unsafe {
        libc::umask(previous_umask);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_socket(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codex-micro-service-{name}-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn lit_zone(zone: &str) -> Lighting {
        Lighting {
            aggregate: BTreeMap::from([(
                zone.into(),
                Light {
                    c: 0xFF_0000,
                    b: 1.0,
                    e: 1,
                    s: 0.5,
                },
            )]),
            ..Lighting::default()
        }
    }

    fn blanked_zone(zone: &str) -> Lighting {
        Lighting {
            aggregate: BTreeMap::from([(zone.into(), Light::default())]),
            ..Lighting::default()
        }
    }

    #[test]
    fn protocol_and_keymap_frames_are_strictly_bounded() {
        assert!(encoded_keymap_limit() + 4096 < MAX_FRAME_BYTES);

        // Pins the wire tag: the installed service binary can lag the client.
        let authorization = serde_json::to_value(ClientMessage::Call {
            protocol: PROTOCOL_VERSION,
            id: 1,
            operation: Operation::Authorize,
        })
        .unwrap();
        assert_eq!(authorization["operation"]["type"], "authorize");

        let mut oversized = io::Cursor::new(vec![b'x'; MAX_FRAME_BYTES + 1]);
        assert!(read_line(&mut oversized).is_err());
    }

    #[test]
    fn socket_is_private_and_same_user_peers_pass() {
        let path = temp_socket("private");
        let (listener, cleanup) = bind_socket(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let (client, _server) = UnixStream::pair().unwrap();
        require_peer_uid(&client, effective_uid()).unwrap();
        drop(listener);
        drop(cleanup);
        assert!(!path.exists());
    }

    #[test]
    fn payload_validation_and_controller_lifetime_are_explicit() {
        assert!(validate_lighting(&Lighting::default()).is_ok());
        assert!(
            validate_focused_app(&FocusedApp {
                app_name: "".into(),
                process: "device.layer-2".into(),
            })
            .is_err()
        );
        assert!(
            validate_lighting(&Lighting {
                aggregate: BTreeMap::new(),
                slots: vec![Light {
                    b: f64::NAN,
                    ..Light::default()
                }],
            })
            .is_err()
        );

        let (event_tx, _event_rx) = mpsc::channel();
        let (handy_tx, _handy_rx) = mpsc::channel();
        let mut state = ServiceState::new(event_tx, handy_tx, None);
        assert!(state.call(1, Operation::Acquire, Instant::now()).is_err());
        assert_eq!(state.controller, None);
        state.controller = Some(1);
        state.focused_app = Some(FocusedApp {
            app_name: "Client".into(),
            process: "dev.client".into(),
        });
        state.lighting = Some(lit_zone("ambient"));
        let (events, _events_rx) = mpsc::sync_channel(1);
        let (shutdown, _peer) = UnixStream::pair().unwrap();
        state
            .clients
            .insert(2, ClientConnection { events, shutdown });
        assert!(
            state
                .call(2, Operation::Acquire, Instant::now() + CALL_TIMEOUT)
                .is_err()
        );
        state.next_retry = Instant::now() + Duration::from_secs(60);
        state.handle(ServiceCommand::Disconnected { client: 1 });
        assert_eq!(state.controller, None);
        assert_eq!(state.focused_app, Some(layer_identity(1)));
        assert_eq!(state.lighting, Some(blanked_zone("ambient")));
        assert!(state.next_retry <= Instant::now());
    }

    #[test]
    fn external_owner_transition_blocks_then_resumes_retry() {
        let (event_tx, _event_rx) = mpsc::channel();
        let (handy_tx, _handy_rx) = mpsc::channel();
        let mut state = ServiceState::new(event_tx, handy_tx, None);
        state.next_retry = Instant::now() + Duration::from_secs(60);

        state.refresh_external_owner(Some(ExternalOwner::Input));
        assert_eq!(state.status().external_owner, Some(ExternalOwner::Input));
        assert!(state.device.is_none());

        state.refresh_external_owner(None);
        assert_eq!(state.status().external_owner, None);
        assert!(state.next_retry <= Instant::now());
    }

    #[test]
    fn failed_event_writer_runs_controller_cleanup() {
        let (event_tx, _event_rx) = mpsc::channel();
        let (handy_tx, _handy_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::sync_channel(1);
        drop(events_rx);
        let (shutdown, mut peer) = UnixStream::pair().unwrap();
        let mut state = ServiceState::new(event_tx, handy_tx, None);
        state.clients.insert(
            1,
            ClientConnection {
                events: events_tx,
                shutdown,
            },
        );
        state.controller = Some(1);
        state.focused_app = Some(FocusedApp {
            app_name: "Client".into(),
            process: "dev.client".into(),
        });
        state.lighting = Some(lit_zone("keys"));
        state.next_retry = Instant::now() + Duration::from_secs(60);

        state.device_event_with_owner(
            DeviceEvent::Key {
                key: "ACT1".into(),
                action: 1,
            },
            None,
        );

        assert_eq!(state.controller, None);
        assert!(!state.clients.contains_key(&1));
        assert_eq!(state.focused_app, Some(layer_identity(1)));
        assert_eq!(state.lighting, Some(blanked_zone("keys")));
        assert!(state.next_retry <= Instant::now());
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn removed_client_cannot_acquire_cached_device() {
        let (event_tx, _event_rx) = mpsc::channel();
        let (handy_tx, _handy_rx) = mpsc::channel();
        let mut state = ServiceState::new(event_tx, handy_tx, None);
        state.device_info = Some(DeviceInfo {
            transport: crate::Transport::Usb,
            firmware: "test".into(),
        });

        assert!(
            state
                .call(7, Operation::Acquire, Instant::now() + CALL_TIMEOUT)
                .is_err()
        );
        assert_eq!(state.controller, None);
    }

    #[test]
    fn full_event_queue_forces_client_disconnect() {
        let (event_tx, _event_rx) = mpsc::channel();
        let (handy_tx, _handy_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::sync_channel(1);
        events_tx
            .send(ServerMessage::Event {
                protocol: PROTOCOL_VERSION,
                event: DeviceEvent::Key {
                    key: "ACT2".into(),
                    action: 1,
                },
            })
            .unwrap();
        let (shutdown, mut peer) = UnixStream::pair().unwrap();
        let mut state = ServiceState::new(event_tx, handy_tx, None);
        state.clients.insert(
            1,
            ClientConnection {
                events: events_tx,
                shutdown,
            },
        );
        state.controller = Some(1);

        state.device_event_with_owner(
            DeviceEvent::Key {
                key: "ACT1".into(),
                action: 1,
            },
            None,
        );

        assert_eq!(state.controller, None);
        assert!(!state.clients.contains_key(&1));
        assert!(events_rx.try_recv().is_ok());
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn act10_is_local_and_suppressed() {
        let (device_tx, _device_rx) = mpsc::channel();
        let (handy_tx, handy_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::sync_channel(2);
        let (shutdown, _peer) = UnixStream::pair().unwrap();
        let mut state = ServiceState::new(device_tx, handy_tx, None);
        state.handle(ServiceCommand::Connected {
            client: 1,
            events: events_tx,
            shutdown,
        });
        state.controller = Some(1);

        state.device_event_with_owner(
            DeviceEvent::Key {
                key: "ACT10".into(),
                action: 1,
            },
            None,
        );
        state.device_event_with_owner(
            DeviceEvent::Key {
                key: "ACT10".into(),
                action: 0,
            },
            None,
        );

        assert_eq!(handy_rx.try_iter().count(), 1);
        assert!(events_rx.try_recv().is_err());
    }

    #[test]
    fn external_owner_suppresses_cached_act10() {
        let (device_tx, _device_rx) = mpsc::channel();
        let (handy_tx, handy_rx) = mpsc::channel();
        let mut state = ServiceState::new(device_tx, handy_tx, Some(ExternalOwner::Input));

        state.device_event_with_owner(
            DeviceEvent::Key {
                key: "ACT10".into(),
                action: 1,
            },
            Some(ExternalOwner::Input),
        );

        assert!(handy_rx.try_recv().is_err());
        assert_eq!(state.external_owner, Some(ExternalOwner::Input));
    }

    #[test]
    fn queued_input_is_delivered_before_controller_handoff() {
        let (device_tx, device_rx) = mpsc::channel();
        let (handy_tx, _handy_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::sync_channel(1);
        let (shutdown, _peer) = UnixStream::pair().unwrap();
        let mut state = ServiceState::new(device_tx.clone(), handy_tx, None);
        state.handle(ServiceCommand::Connected {
            client: 1,
            events: events_tx,
            shutdown,
        });
        state.controller = Some(1);
        device_tx
            .send(DeviceEvent::Key {
                key: "ACT1".into(),
                action: 1,
            })
            .unwrap();

        handle_command_after_events(
            &mut state,
            ServiceCommand::Disconnected { client: 1 },
            &device_rx,
            || None,
        );

        assert!(matches!(
            events_rx.try_recv().unwrap(),
            ServerMessage::Event {
                event: DeviceEvent::Key { ref key, action: 1 },
                ..
            } if key == "ACT1"
        ));
        assert_eq!(state.controller, None);
    }

    #[test]
    fn close_intent_suppresses_disconnect_event() {
        let (stream, peer) = UnixStream::pair().unwrap();
        drop(peer);
        let (event_tx, event_rx) = mpsc::channel();
        let pending = Arc::new(Mutex::new(None));
        let closed = Arc::new(AtomicBool::new(false));
        let closing = Arc::new(AtomicBool::new(true));

        client_reader(
            BufReader::new(stream),
            event_tx,
            pending,
            Arc::clone(&closed),
            closing,
        );

        assert!(closed.load(Ordering::Acquire));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn service_lock_excludes_a_second_server() {
        let path = temp_socket("lock");
        let first = ServiceLock::acquire(&path).unwrap();
        assert!(ServiceLock::acquire(&path).is_err());
        drop(first);
        ServiceLock::acquire(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ServiceLock::acquire(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = path.with_extension("link");
        fs::hard_link(&path, &link).unwrap();
        assert!(ServiceLock::acquire(&path).is_err());
        fs::remove_file(link).unwrap();
        fs::remove_file(path).unwrap();
    }
}
