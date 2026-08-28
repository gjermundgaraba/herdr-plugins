//! Shared IOKit HID transport for the Work Louder Codex Micro.
//!
//! Two threads touch IOKit: the owner thread runs the run loop, callbacks,
//! and lifecycle, and a writer thread makes the synchronous set-report calls.
//! The public handle only sends commands and waits for replies, which keeps a
//! BLE reconnect from leaving callbacks pointed at a moved or dropped
//! context.

use std::{
    collections::HashMap,
    ffi::{CStr, c_void},
    fs::{File, OpenOptions},
    io,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::Path,
    ptr::NonNull,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRunLoop, CFString, kCFRunLoopDefaultMode};
use objc2_io_kit::{
    IOHIDAccessType, IOHIDCheckAccess, IOHIDDevice, IOHIDManager, IOHIDReportType,
    IOHIDRequestAccess, IOHIDRequestType, IOOptionBits, IOReturn, kIOHIDLocationIDKey,
    kIOHIDProductIDKey, kIOHIDSerialNumberKey, kIOHIDTransportKey, kIOHIDVendorIDKey,
    kIOReturnSuccess,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::wire::{REPORT_ID, REPORT_SIZE, Reassembler, encode_message};

const MICRO_VENDOR_ID: i32 = 0x303A;
const MICRO_PRODUCT_ID: i32 = 0x8360;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const RESPONSE_SLACK: Duration = Duration::from_millis(100);
const DEVICE_OPEN_TIMEOUT: Duration = Duration::from_secs(7);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InputMonitoringAccess {
    Granted,
    Denied,
    Unknown,
}

pub(crate) fn input_monitoring_access() -> InputMonitoringAccess {
    match IOHIDCheckAccess(IOHIDRequestType::ListenEvent) {
        IOHIDAccessType::Granted => InputMonitoringAccess::Granted,
        IOHIDAccessType::Denied => InputMonitoringAccess::Denied,
        _ => InputMonitoringAccess::Unknown,
    }
}

pub(crate) fn input_monitoring_request_access() -> InputMonitoringAccess {
    if IOHIDRequestAccess(IOHIDRequestType::ListenEvent) {
        InputMonitoringAccess::Granted
    } else {
        input_monitoring_access()
    }
}

// Variant order is the device-open preference order: USB first.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Transport {
    Usb,
    BluetoothLowEnergy,
    Other,
}

impl Transport {
    fn from_iokit(value: &str) -> Self {
        match value {
            "USB" => Self::Usb,
            "Bluetooth Low Energy" => Self::BluetoothLowEnergy,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub transport: Transport,
    pub firmware: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DeviceEvent {
    Key { key: String, action: i64 },
    Joystick { angle: f64, distance: f64 },
    Disconnected { error: String },
}

enum CallbackEvent {
    Report(Vec<u8>),
    WriteComplete(IOReturn),
    Removed,
    Failed(String),
}

/// Stable for the full registration lifetime; IOKit retains only this pointer.
struct CallbackContext {
    callback_tx: Sender<CallbackEvent>,
}

enum Command {
    Send {
        method: String,
        params: Option<Value>,
        deadline: Instant,
        reply: SyncSender<std::result::Result<(), String>>,
    },
    Request {
        id: u64,
        method: String,
        params: Option<Value>,
        deadline: Instant,
        reply: SyncSender<std::result::Result<Value, String>>,
    },
    Close,
}

struct Pending {
    deadline: Instant,
    reply: SyncSender<std::result::Result<Value, String>>,
}

pub struct MicroDevice {
    command_tx: Sender<Command>,
    next_request_id: AtomicU64,
    closed: Arc<AtomicBool>,
    terminal_error: Arc<OnceLock<String>>,
    owner: Option<JoinHandle<Result<()>>>,
    lock: Option<DeviceLock>,
}

impl MicroDevice {
    /// Opens the best available Micro and does the required `device.status`
    /// round trip before returning it.
    pub fn open(event_tx: Sender<DeviceEvent>) -> Result<(Self, DeviceInfo)> {
        if input_monitoring_access() == InputMonitoringAccess::Denied {
            bail!("Input Monitoring access is denied")
        }
        let lock = DeviceLock::acquire()?;
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let terminal_error = Arc::new(OnceLock::new());
        let owner_closed = Arc::clone(&closed);
        let owner_terminal_error = Arc::clone(&terminal_error);
        let failure_closed = Arc::clone(&closed);
        let failure_terminal_error = Arc::clone(&terminal_error);
        let owner_event_tx = event_tx.clone();
        let failure_ready_tx = ready_tx.clone();
        let owner = thread::Builder::new()
            .name("codex-micro-hid".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    owner_main(
                        command_rx,
                        event_tx,
                        ready_tx,
                        owner_closed,
                        owner_terminal_error,
                    )
                }));
                let result = match result {
                    Ok(result) => result,
                    Err(_) => Err(anyhow!("Micro HID owner thread panicked")),
                };
                if let Err(error) = &result {
                    let error = failure_terminal_error
                        .get_or_init(|| error.to_string())
                        .clone();
                    failure_closed.store(true, Ordering::Release);
                    let _ = failure_ready_tx.try_send(Err(error.clone()));
                    let _ = owner_event_tx.send(DeviceEvent::Disconnected { error });
                }
                result
            })?;

        match ready_rx.recv_timeout(DEVICE_OPEN_TIMEOUT) {
            Ok(Ok(info)) => Ok((
                Self {
                    command_tx,
                    next_request_id: AtomicU64::new(1),
                    closed,
                    terminal_error,
                    owner: Some(owner),
                    lock: Some(lock),
                },
                info,
            )),
            Ok(Err(error)) => {
                let _ = owner.join();
                Err(anyhow!(error))
            }
            Err(_) => {
                let _ = command_tx.send(Command::Close);
                let _ = owner.join();
                Err(anyhow!("Codex Micro not found or unavailable"))
            }
        }
    }

    pub fn send(&self, method: impl Into<String>, params: Option<Value>) -> Result<()> {
        self.ensure_open()?;
        let deadline = Instant::now() + DEFAULT_REQUEST_TIMEOUT;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.command_tx
            .send(Command::Send {
                method: method.into(),
                params,
                deadline,
                reply: reply_tx,
            })
            .map_err(|_| self.disconnected_error())?;
        await_reply(
            reply_rx,
            DEFAULT_REQUEST_TIMEOUT + RESPONSE_SLACK,
            &self.terminal_error,
            || anyhow!("device write timed out"),
        )
    }

    pub fn request(&self, method: impl Into<String>, params: Option<Value>) -> Result<Value> {
        self.ensure_open()?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let deadline = Instant::now() + DEFAULT_REQUEST_TIMEOUT;
        self.command_tx
            .send(Command::Request {
                id,
                method: method.into(),
                params,
                deadline,
                reply: reply_tx,
            })
            .map_err(|_| self.disconnected_error())?;
        await_reply(
            reply_rx,
            DEFAULT_REQUEST_TIMEOUT + RESPONSE_SLACK,
            &self.terminal_error,
            || anyhow!("request {id} timed out"),
        )
    }

    pub fn close(&mut self) -> Result<()> {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let _ = self.command_tx.send(Command::Close);
        }
        let joined = match self.owner.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(anyhow!("Micro HID owner thread panicked")),
            None => Ok(()),
        };
        self.lock.take();
        joined
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(self.disconnected_error());
        }
        Ok(())
    }

    fn disconnected_error(&self) -> anyhow::Error {
        anyhow!(
            self.terminal_error
                .get()
                .map_or("device disconnected", String::as_str)
                .to_owned()
        )
    }
}

fn await_reply<T>(
    reply_rx: Receiver<std::result::Result<T, String>>,
    timeout: Duration,
    terminal_error: &OnceLock<String>,
    timeout_error: impl FnOnce() -> anyhow::Error,
) -> Result<T> {
    reply_rx
        .recv_timeout(timeout)
        .map_err(|_| {
            terminal_error
                .get()
                .cloned()
                .map(anyhow::Error::msg)
                .unwrap_or_else(timeout_error)
        })?
        .map_err(|error| anyhow!(error))
}

/// Dropping the owned file closes its descriptor, which releases the flock.
struct DeviceLock(#[allow(dead_code)] File);

impl DeviceLock {
    fn acquire() -> Result<Self> {
        let uid = unsafe { libc::getuid() };
        // Stable across app updates so upgrades cannot acquire two locks.
        Self::acquire_at(&Path::new("/tmp").join(format!("dev.codex-micro-{uid}.lock")))
    }

    fn acquire_at(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::getuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o7777 != 0o600
        {
            bail!("unsafe Codex Micro device lock {}", path.display())
        }
        // SAFETY: flock only operates on this live file descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                bail!("another process owns the Codex Micro")
            }
            return Err(error.into());
        }
        Ok(Self(file))
    }
}

impl Drop for MicroDevice {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn owner_main(
    command_rx: Receiver<Command>,
    event_tx: Sender<DeviceEvent>,
    ready_tx: SyncSender<std::result::Result<DeviceInfo, String>>,
    closed: Arc<AtomicBool>,
    terminal_error: Arc<OnceLock<String>>,
) -> Result<()> {
    let result = (|| {
        let (mut owner, info) = Owner::open(
            Arc::new(Mutex::new(command_rx)),
            event_tx,
            closed,
            terminal_error,
        )?;
        if ready_tx.send(Ok(info)).is_err() {
            owner.teardown(true);
            bail!("opener dropped")
        }
        owner.run()
    })();
    if let Err(error) = &result {
        let _ = ready_tx.send(Err(error.to_string()));
    }
    result
}

struct Owner {
    manager: objc2_core_foundation::CFRetained<IOHIDManager>,
    device: objc2_core_foundation::CFRetained<IOHIDDevice>,
    transport: Transport,
    run_loop: objc2_core_foundation::CFRetained<CFRunLoop>,
    run_loop_mode: &'static CFString,
    input_buffer: Box<[u8; REPORT_SIZE]>,
    // Heap-allocated so the foreign callback pointer stays stable.
    context: Box<CallbackContext>,
    callback_rx: Receiver<CallbackEvent>,
    writer_tx: Sender<Vec<u8>>,
    command_rx: Arc<Mutex<Receiver<Command>>>,
    event_tx: Sender<DeviceEvent>,
    reassembler: Reassembler,
    pending: HashMap<u64, Pending>,
    closed: Arc<AtomicBool>,
    terminal_error: Arc<OnceLock<String>>,
    announced: bool,
    torn_down: bool,
}

impl Owner {
    fn open(
        command_rx: Arc<Mutex<Receiver<Command>>>,
        event_tx: Sender<DeviceEvent>,
        closed: Arc<AtomicBool>,
        terminal_error: Arc<OnceLock<String>>,
    ) -> Result<(Self, DeviceInfo)> {
        let manager = IOHIDManager::new(None, 0 as IOOptionBits);
        let vendor_key = cf_string(kIOHIDVendorIDKey);
        let product_key = cf_string(kIOHIDProductIDKey);
        let vendor = CFNumber::new_i32(MICRO_VENDOR_ID);
        let product = CFNumber::new_i32(MICRO_PRODUCT_ID);
        let matching = CFDictionary::<CFString, CFNumber>::from_slices(
            &[&vendor_key, &product_key],
            &[&vendor, &product],
        );
        // SAFETY: Both keys and values are the documented IOKit CF types and
        // `matching` remains retained until IOKit has copied its criteria.
        unsafe { manager.set_device_matching(Some(matching.as_opaque())) };
        if manager.open(0) != kIOReturnSuccess {
            bail!("Codex Micro not found or unavailable")
        }

        let candidates = choose_devices(&manager)?;
        let mut last_error = None;
        for candidate in candidates {
            match Self::open_candidate(
                manager.clone(),
                candidate,
                Arc::clone(&command_rx),
                event_tx.clone(),
                Arc::clone(&closed),
                Arc::clone(&terminal_error),
            ) {
                Ok(mut owner) => match owner.handshake() {
                    Ok(info) => {
                        owner.announced = true;
                        return Ok((owner, info));
                    }
                    Err(error) => {
                        owner.teardown(false);
                        closed.store(false, Ordering::Release);
                        last_error = Some(error);
                    }
                },
                Err(error) => last_error = Some(error),
            }
        }
        let _ = manager.close(0);
        Err(last_error.unwrap_or_else(|| anyhow!("Codex Micro not found or unavailable")))
    }

    fn open_candidate(
        manager: objc2_core_foundation::CFRetained<IOHIDManager>,
        candidate: DeviceCandidate,
        command_rx: Arc<Mutex<Receiver<Command>>>,
        event_tx: Sender<DeviceEvent>,
        closed: Arc<AtomicBool>,
        terminal_error: Arc<OnceLock<String>>,
    ) -> Result<Self> {
        let DeviceCandidate {
            device, transport, ..
        } = candidate;
        let run_loop =
            CFRunLoop::current().ok_or_else(|| anyhow!("no Core Foundation run loop"))?;
        // kCFRunLoopDefaultMode is supplied by CoreFoundation for the process lifetime.
        let run_loop_mode = unsafe { kCFRunLoopDefaultMode }
            .ok_or_else(|| anyhow!("no Core Foundation default run loop mode"))?;
        if device.open(0) != kIOReturnSuccess {
            bail!("Codex Micro is unavailable")
        }
        let (callback_tx, callback_rx) = mpsc::channel();
        let (writer_tx, writer_rx) = mpsc::channel();
        let writer_device = WriterDevice(device.clone());
        let writer_callback_tx = callback_tx.clone();
        thread::Builder::new()
            .name("codex-micro-hid-writer".into())
            .spawn(move || writer_main(writer_device, writer_rx, writer_callback_tx))?;
        let context = Box::new(CallbackContext { callback_tx });
        let mut input_buffer = Box::new([0; REPORT_SIZE]);
        let buffer = NonNull::from(input_buffer.as_mut()).cast::<u8>();
        let context_ptr = &*context as *const CallbackContext as *mut c_void;
        // SAFETY: the `input_buffer` and `context` allocations outlive
        // registration; teardown unregisters before either is dropped.
        unsafe {
            device.register_input_report_callback(
                buffer,
                REPORT_SIZE as isize,
                Some(input_report_callback),
                context_ptr,
            );
            device.register_removal_callback(Some(removal_callback), context_ptr);
            device.schedule_with_run_loop(&run_loop, run_loop_mode);
        }

        Ok(Self {
            manager,
            device,
            transport,
            run_loop,
            run_loop_mode,
            input_buffer,
            context,
            callback_rx,
            writer_tx,
            command_rx,
            event_tx,
            reassembler: Reassembler::default(),
            pending: HashMap::new(),
            closed,
            terminal_error,
            announced: false,
            torn_down: false,
        })
    }

    fn handshake(&mut self) -> Result<DeviceInfo> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let deadline = Instant::now() + DEFAULT_REQUEST_TIMEOUT;
        self.start_request(0, "device.status".into(), None, deadline, reply_tx)?;
        while Instant::now() < deadline {
            self.pump();
            match reply_rx.try_recv() {
                Ok(Ok(status)) => return device_info(self.transport, &status),
                Ok(Err(error)) => bail!(error),
                Err(TryRecvError::Disconnected) => bail!("device.status timed out"),
                Err(TryRecvError::Empty) => {}
            }
        }
        bail!("device.status timed out")
    }

    fn run(&mut self) -> Result<()> {
        loop {
            self.pump();
            if self.closed.load(Ordering::Acquire) {
                break;
            }
            let command = match self.command_rx.lock() {
                Ok(receiver) => receiver.try_recv(),
                Err(_) => break,
            };
            match command {
                Ok(Command::Close) | Err(TryRecvError::Disconnected) => break,
                Ok(Command::Send {
                    method,
                    params,
                    deadline,
                    reply,
                }) => {
                    let result = if Instant::now() >= deadline {
                        Err(anyhow!("device write timed out"))
                    } else {
                        self.write(method, params, None, deadline)
                    };
                    let _ = reply.send(result.map_err(|error| error.to_string()));
                }
                Ok(Command::Request {
                    id,
                    method,
                    params,
                    deadline,
                    reply,
                }) => {
                    let _ = self.start_request(id, method, params, deadline, reply);
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let terminal_error = self.terminal_error.get().cloned();
        self.teardown(true);
        terminal_error.map(anyhow::Error::msg).map_or(Ok(()), Err)
    }

    fn pump(&mut self) {
        // Running a short turn lets CoreFoundation dispatch HID callbacks while
        // still giving channel commands and request timeouts predictable latency.
        let _ = self.pump_for(Duration::from_millis(10));
    }

    fn pump_for(&mut self, duration: Duration) -> Option<IOReturn> {
        let _ = CFRunLoop::run_in_mode(Some(self.run_loop_mode), duration.as_secs_f64(), true);
        let mut write_status = None;
        while let Ok(event) = self.callback_rx.try_recv() {
            match event {
                CallbackEvent::Report(bytes) => self.handle_report(&bytes),
                CallbackEvent::WriteComplete(status) => write_status = Some(status),
                CallbackEvent::Removed => self.disconnect("Codex Micro disconnected".into()),
                CallbackEvent::Failed(error) => self.disconnect(error),
            }
        }
        let now = Instant::now();
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(&id, pending)| (pending.deadline <= now).then_some(id))
            .collect();
        for id in expired {
            self.fail_pending(id, format!("request {id} timed out"));
        }
        write_status
    }

    fn start_request(
        &mut self,
        id: u64,
        method: String,
        params: Option<Value>,
        deadline: Instant,
        reply: SyncSender<std::result::Result<Value, String>>,
    ) -> Result<()> {
        self.pending.insert(id, Pending { deadline, reply });
        if Instant::now() >= deadline {
            self.fail_pending(id, format!("request {id} timed out"));
            return Ok(());
        }
        if let Err(error) = self.write(method, params, Some(id), deadline) {
            self.fail_pending(id, error.to_string());
            return Err(error);
        }
        Ok(())
    }

    fn write(
        &mut self,
        method: String,
        params: Option<Value>,
        id: Option<u64>,
        deadline: Instant,
    ) -> Result<()> {
        let mut wrote_report = false;
        for report in encode_message(&method, params.as_ref(), id)? {
            if Instant::now() >= deadline {
                let error = "device write timed out";
                if wrote_report {
                    self.disconnect(error.into());
                }
                bail!(error)
            }
            if self
                .writer_tx
                .send(output_wire(&report, self.transport)?)
                .is_err()
            {
                let error = "device writer stopped";
                self.disconnect(error.into());
                bail!(error)
            }
            let status = loop {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    let error = "device write timed out";
                    self.disconnect(error.into());
                    bail!(error)
                };
                if let Some(status) = self.pump_for(remaining.min(Duration::from_millis(10))) {
                    break status;
                }
                if self.closed.load(Ordering::Acquire) {
                    bail!(
                        self.terminal_error
                            .get()
                            .map_or("device disconnected", String::as_str)
                            .to_owned()
                    )
                }
            };
            if self.closed.load(Ordering::Acquire) {
                bail!(
                    self.terminal_error
                        .get()
                        .map_or("device disconnected", String::as_str)
                        .to_owned()
                )
            }
            if status != kIOReturnSuccess {
                let error = format!("IOHIDDeviceSetReport failed: 0x{:08X}", status as u32);
                self.disconnect(error.clone());
                bail!(error)
            }
            wrote_report = true;
        }
        if Instant::now() >= deadline {
            let error = "device write timed out";
            self.disconnect(error.into());
            bail!(error)
        }
        Ok(())
    }

    fn handle_report(&mut self, report: &[u8]) {
        for envelope in self.reassembler.push(report) {
            let Ok(envelope) = envelope else { continue };
            if let Some(id) = envelope.get("id").and_then(Value::as_u64)
                && (envelope.get("result").is_some() || envelope.get("error").is_some())
            {
                if let Some(pending) = self.pending.remove(&id) {
                    let result = envelope
                        .get("error")
                        .map(error_message)
                        .map(Err)
                        .unwrap_or_else(|| {
                            Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
                        });
                    let _ = pending.reply.send(result);
                }
                continue;
            }
            if let Some(event) = parse_event(&envelope) {
                let _ = self.event_tx.send(event);
            }
        }
    }

    fn fail_pending(&mut self, id: u64, error: String) {
        if let Some(pending) = self.pending.remove(&id) {
            let _ = pending.reply.send(Err(error));
        }
    }

    fn fail_all(&mut self, error: &str) {
        for (_, pending) in self.pending.drain() {
            let _ = pending.reply.send(Err(error.into()));
        }
    }

    fn disconnect(&mut self, error: String) {
        let error = if self.announced {
            self.terminal_error.get_or_init(|| error).clone()
        } else {
            error
        };
        self.closed.store(true, Ordering::Release);
        self.fail_all(&error);
        if let Ok(receiver) = self.command_rx.lock() {
            fail_queued(&receiver, &error);
        }
    }

    fn teardown(&mut self, close_manager: bool) {
        if self.torn_down {
            return;
        }
        self.closed.store(true, Ordering::Release);
        let error = self
            .terminal_error
            .get()
            .map(String::as_str)
            .unwrap_or("device disconnected")
            .to_owned();
        self.fail_all(&error);
        if let Ok(receiver) = self.command_rx.lock() {
            fail_queued(&receiver, &error);
        }
        // SAFETY: This is the same owner thread that registered and scheduled
        // the callbacks. Clearing them precedes unscheduling, close, and drop.
        let context_ptr = &*self.context as *const CallbackContext as *mut c_void;
        unsafe {
            self.device.register_input_report_callback(
                NonNull::from(self.input_buffer.as_mut()).cast(),
                REPORT_SIZE as isize,
                None,
                context_ptr,
            );
            self.device.register_removal_callback(None, context_ptr);
            self.device
                .unschedule_from_run_loop(&self.run_loop, self.run_loop_mode);
        }
        let _ = self.device.close(0);
        if close_manager {
            let _ = self.manager.close(0);
        }
        self.torn_down = true;
        // `context` drops only after IOKit no longer has a callback registration.
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.teardown(true);
    }
}

unsafe extern "C-unwind" fn input_report_callback(
    context: *mut c_void,
    result: IOReturn,
    _sender: *mut c_void,
    report_type: IOHIDReportType,
    report_id: u32,
    report: NonNull<u8>,
    length: isize,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: IOKit keeps the registered context alive for this callback.
    let context = unsafe { &*(context.cast::<CallbackContext>()) };
    if result != kIOReturnSuccess {
        let _ = context.callback_tx.send(CallbackEvent::Failed(format!(
            "input report failed: 0x{:08X}",
            result as u32
        )));
        return;
    }
    if report_type != IOHIDReportType::Input || report_id != REPORT_ID as u32 || length <= 0 {
        return;
    }
    let length = usize::try_from(length).unwrap_or(0).min(REPORT_SIZE);
    // SAFETY: IOKit guarantees `report` points to `length` received bytes for
    // this callback; `context` stays alive until callbacks are unregistered.
    let bytes = unsafe { std::slice::from_raw_parts(report.as_ptr(), length) }.to_vec();
    let _ = context.callback_tx.send(CallbackEvent::Report(bytes));
}

/// The writer thread's retained device handle. Writes use the synchronous
/// `IOHIDDeviceSetReport` because the asynchronous
/// `IOHIDDeviceSetReportWithCallback` stalls macOS input report delivery on a
/// long-lived handle (async-hid #45); hidapi uses the same synchronous call.
/// Teardown may close the device while a write is still blocked, and Apple
/// documents no outcome: if close returns without unblocking the write, the
/// stuck write leaks this thread and its device retain (one per reopen at
/// worst); if close itself blocks on the write, teardown wedges. Both are
/// accepted over running the uncancellable call on the owner thread, where
/// any stuck write would wedge every shutdown.
struct WriterDevice(objc2_core_foundation::CFRetained<IOHIDDevice>);

// SAFETY: The wrapped device is only used for synchronous set-report calls on
// the one writer thread. Concurrency with run-loop input delivery on the
// owner thread is the steady-state shape hidapi uses on macOS; close during
// teardown is beyond that comparison and is the undocumented teardown risk
// described above.
unsafe impl Send for WriterDevice {}

fn writer_main(device: WriterDevice, jobs: Receiver<Vec<u8>>, callback_tx: Sender<CallbackEvent>) {
    while let Ok(wire) = jobs.recv() {
        let pointer = NonNull::from(wire.as_slice()).cast::<u8>();
        // SAFETY: `wire` outlives this synchronous call and its length is exact.
        let status = unsafe {
            device.0.set_report(
                IOHIDReportType::Output,
                REPORT_ID as isize,
                pointer,
                wire.len() as isize,
            )
        };
        if callback_tx
            .send(CallbackEvent::WriteComplete(status))
            .is_err()
        {
            return;
        }
    }
}

unsafe extern "C-unwind" fn removal_callback(
    context: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: Registration keeps this context alive until unregistered.
    let context = unsafe { &*(context.cast::<CallbackContext>()) };
    let _ = context.callback_tx.send(CallbackEvent::Removed);
}

struct DeviceCandidate {
    device: objc2_core_foundation::CFRetained<IOHIDDevice>,
    transport: Transport,
    location: Option<i64>,
    serial: String,
}

fn choose_devices(manager: &IOHIDManager) -> Result<Vec<DeviceCandidate>> {
    let devices = manager
        .devices()
        .ok_or_else(|| anyhow!("Codex Micro not found or unavailable"))?;
    let count = devices.count();
    if count <= 0 {
        bail!("Codex Micro not found or unavailable")
    }
    let mut pointers: Vec<*const c_void> = vec![std::ptr::null(); count as usize];
    // SAFETY: `pointers` has one entry per CFSet member; IOHIDManager returns
    // only IOHIDDevice objects for this set, which are retained before return.
    unsafe { devices.values(pointers.as_mut_ptr()) };
    let location_key = cf_string(kIOHIDLocationIDKey);
    let serial_key = cf_string(kIOHIDSerialNumberKey);
    let mut candidates = Vec::new();
    for pointer in pointers {
        let Some(pointer) = NonNull::new(pointer.cast_mut()) else {
            continue;
        };
        // SAFETY: the CFSet is documented to contain IOHIDDevice values and
        // remains retained while this newly retained device handle is created.
        let device =
            unsafe { objc2_core_foundation::CFRetained::retain(pointer.cast::<IOHIDDevice>()) };
        let location = device
            .property(&location_key)
            .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i64));
        let serial = device
            .property(&serial_key)
            .and_then(|value| value.downcast_ref::<CFString>().map(ToString::to_string))
            .unwrap_or_default();
        candidates.push(DeviceCandidate {
            transport: device_transport(&device),
            device,
            location,
            serial,
        });
    }
    candidates.sort_by(|left, right| {
        (left.transport, left.location, left.serial.as_str()).cmp(&(
            right.transport,
            right.location,
            right.serial.as_str(),
        ))
    });
    if candidates.is_empty() {
        bail!("Codex Micro not found or unavailable")
    }
    Ok(candidates)
}

fn device_transport(device: &IOHIDDevice) -> Transport {
    let key = cf_string(kIOHIDTransportKey);
    device
        .property(&key)
        .and_then(|value| value.downcast_ref::<CFString>().map(ToString::to_string))
        .as_deref()
        .map(Transport::from_iokit)
        .unwrap_or(Transport::Other)
}

fn cf_string(value: &CStr) -> objc2_core_foundation::CFRetained<CFString> {
    CFString::from_str(value.to_str().expect("IOKit keys are UTF-8"))
}

fn output_wire(report: &[u8; REPORT_SIZE], transport: Transport) -> Result<Vec<u8>> {
    if report[0] != REPORT_ID {
        bail!("invalid Micro report ID")
    }
    Ok(if transport == Transport::Usb {
        report[1..].to_vec()
    } else {
        report.to_vec()
    })
}

fn device_info(transport: Transport, status: &Value) -> Result<DeviceInfo> {
    let firmware = status
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|firmware| !firmware.is_empty())
        .ok_or_else(|| anyhow!("device did not report a firmware version"))?;
    Ok(DeviceInfo {
        transport,
        firmware: firmware.to_owned(),
    })
}

fn fail_queued(command_rx: &Receiver<Command>, error: &str) {
    while let Ok(command) = command_rx.try_recv() {
        match command {
            Command::Send { reply, .. } => {
                let _ = reply.send(Err(error.into()));
            }
            Command::Request { reply, .. } => {
                let _ = reply.send(Err(error.into()));
            }
            Command::Close => {}
        }
    }
}

fn parse_event(envelope: &Value) -> Option<DeviceEvent> {
    let method = envelope.get("m")?.as_str()?;
    let params = envelope.get("p")?;
    match method {
        "v.oai.hid" => Some(DeviceEvent::Key {
            key: params.get("k")?.as_str()?.to_owned(),
            action: params.get("act")?.as_i64()?,
        }),
        "v.oai.rad" => Some(DeviceEvent::Joystick {
            angle: params.get("a")?.as_f64()?,
            distance: params.get("d")?.as_f64()?,
        }),
        _ => None,
    }
}

fn error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("device request failed")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usb_omits_only_the_report_id() {
        let mut report = [0; REPORT_SIZE];
        report[0] = REPORT_ID;
        report[1] = 2;
        let usb = output_wire(&report, Transport::Usb).unwrap();
        assert_eq!(usb.len(), 63);
        assert_eq!(usb[0], 2);
        assert_eq!(
            output_wire(&report, Transport::BluetoothLowEnergy)
                .unwrap()
                .len(),
            64
        );
    }

    #[test]
    fn transport_selection_matches_iokit_values() {
        assert_eq!(Transport::from_iokit("USB"), Transport::Usb);
        assert_eq!(
            Transport::from_iokit("Bluetooth Low Energy"),
            Transport::BluetoothLowEnergy
        );
        assert_eq!(Transport::from_iokit("Bluetooth"), Transport::Other);
    }

    #[test]
    fn malformed_or_unrelated_event_is_ignored() {
        assert_eq!(parse_event(&json!({"m": "v.oai.hid", "p": {"k": 4}})), None);
        assert_eq!(
            parse_event(&json!({"method": "v.oai.hid", "params": {"k": "x", "act": 1}})),
            None
        );
        assert_eq!(parse_event(&json!({"m": "other", "p": {}})), None);
        assert_eq!(
            parse_event(&json!({"m": "v.oai.rad", "p": {"a": 1.5, "d": 0.75}})),
            Some(DeviceEvent::Joystick {
                angle: 1.5,
                distance: 0.75
            })
        );
    }

    #[test]
    fn lock_excludes_another_micro_owner() {
        use std::os::unix::fs::PermissionsExt;

        let path =
            std::env::temp_dir().join(format!("codex-micro-lock-test-{}", std::process::id()));
        let first = DeviceLock::acquire_at(&path).unwrap();
        assert!(DeviceLock::acquire_at(&path).is_err());
        drop(first);
        assert!(DeviceLock::acquire_at(&path).is_ok());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(DeviceLock::acquire_at(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = path.with_extension("link");
        std::fs::hard_link(&path, &link).unwrap();
        assert!(DeviceLock::acquire_at(&path).is_err());
        std::fs::remove_file(link).unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn close_joins_an_owner_after_removal() {
        let path =
            std::env::temp_dir().join(format!("codex-micro-close-test-{}", std::process::id()));
        let (command_tx, _command_rx) = mpsc::channel();
        let joined = Arc::new(AtomicBool::new(false));
        let owner_joined = Arc::clone(&joined);
        let mut device = MicroDevice {
            command_tx,
            next_request_id: AtomicU64::new(1),
            closed: Arc::new(AtomicBool::new(true)),
            terminal_error: Arc::new(OnceLock::new()),
            owner: Some(thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                owner_joined.store(true, Ordering::Release);
                Ok(())
            })),
            lock: Some(DeviceLock::acquire_at(&path).unwrap()),
        };
        device.close().unwrap();
        assert!(joined.load(Ordering::Acquire));
        assert!(device.owner.is_none());
        assert!(device.lock.is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn handshake_requires_and_reports_nonempty_firmware() {
        assert_eq!(
            device_info(Transport::Usb, &json!({"version": " 0.6.2 "})).unwrap(),
            DeviceInfo {
                transport: Transport::Usb,
                firmware: "0.6.2".into(),
            }
        );
        assert!(device_info(Transport::Usb, &json!({"version": "  "})).is_err());
        assert!(device_info(Transport::Usb, &json!({})).is_err());
    }

    #[test]
    fn terminal_failure_is_preferred_over_a_late_reply_disconnect() {
        let terminal_error = OnceLock::new();
        terminal_error.set("write failed".into()).unwrap();
        let (reply_tx, reply_rx) = mpsc::sync_channel::<std::result::Result<(), String>>(1);
        drop(reply_tx);

        assert_eq!(
            await_reply(reply_rx, Duration::ZERO, &terminal_error, || panic!(
                "timeout error must stay lazy"
            ))
            .unwrap_err()
            .to_string(),
            "write failed"
        );
    }

    #[test]
    fn queued_commands_receive_the_terminal_failure() {
        let (command_tx, command_rx) = mpsc::channel();
        let (send_tx, send_rx) = mpsc::sync_channel(1);
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        command_tx
            .send(Command::Send {
                method: "first".into(),
                params: None,
                deadline: Instant::now(),
                reply: send_tx,
            })
            .unwrap();
        command_tx
            .send(Command::Request {
                id: 1,
                method: "second".into(),
                params: None,
                deadline: Instant::now(),
                reply: request_tx,
            })
            .unwrap();

        fail_queued(&command_rx, "write failed");

        assert_eq!(send_rx.recv().unwrap(), Err("write failed".into()));
        assert_eq!(request_rx.recv().unwrap(), Err("write failed".into()));
    }
}
