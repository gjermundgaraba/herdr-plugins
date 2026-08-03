//! Direct IOKit transport for the Codex Micro.
//!
//! The owner thread is deliberately the only place that touches IOKit.  The
//! public handle only sends commands and waits for replies, which keeps a BLE
//! reconnect from leaving callbacks pointed at a moved or dropped context.

use std::{
    collections::HashMap,
    ffi::{c_void, CStr},
    fs::{File, OpenOptions},
    io,
    os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    path::Path,
    pin::Pin,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Result};
use objc2_core_foundation::{kCFRunLoopDefaultMode, CFDictionary, CFNumber, CFRunLoop, CFString};
use objc2_io_kit::{
    kIOHIDLocationIDKey, kIOHIDProductIDKey, kIOHIDSerialNumberKey, kIOHIDTransportKey,
    kIOHIDVendorIDKey, kIOReturnSuccess, IOHIDAccessType, IOHIDCheckAccess, IOHIDDevice,
    IOHIDManager, IOHIDReportType, IOHIDRequestType, IOOptionBits, IOReturn,
};
use serde_json::Value;

use crate::protocol::{encode_message, Reassembler, REPORT_ID, REPORT_SIZE};

pub const MICRO_VENDOR_ID: i32 = 0x303A;
pub const MICRO_PRODUCT_ID: i32 = 0x8360;
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMonitoringAccess {
    Granted,
    Denied,
    Unknown,
}

pub fn input_monitoring_access() -> InputMonitoringAccess {
    classify_input_monitoring(IOHIDCheckAccess(IOHIDRequestType::ListenEvent))
}

fn classify_input_monitoring(access: IOHIDAccessType) -> InputMonitoringAccess {
    match access {
        IOHIDAccessType::Granted => InputMonitoringAccess::Granted,
        IOHIDAccessType::Denied => InputMonitoringAccess::Denied,
        _ => InputMonitoringAccess::Unknown,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug, PartialEq)]
pub enum DeviceEvent {
    Key { key: String, action: i64 },
    Joystick { angle: f64, distance: f64 },
    Disconnected,
}

enum CallbackEvent {
    Report(Vec<u8>),
    Removed,
}

/// Stable for the full registration lifetime; IOKit retains only this pointer.
struct CallbackContext {
    callback_tx: Sender<CallbackEvent>,
}

enum Command {
    Send {
        method: String,
        params: Option<Value>,
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
    owner: Option<JoinHandle<()>>,
    lock: Option<DeviceLock>,
}

impl MicroDevice {
    /// Opens the best available Micro and does the required `device.status`
    /// round trip before returning it.
    pub fn open(event_tx: Sender<DeviceEvent>) -> Result<Self> {
        if input_monitoring_access() == InputMonitoringAccess::Denied {
            bail!("Input Monitoring access is denied")
        }
        let lock = DeviceLock::acquire()?;
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let owner_closed = Arc::clone(&closed);
        let owner = thread::Builder::new()
            .name("herdr-micro-hid".into())
            .spawn(move || owner_main(command_rx, event_tx, ready_tx, owner_closed))?;

        match ready_rx.recv_timeout(DEFAULT_REQUEST_TIMEOUT + Duration::from_secs(1)) {
            Ok(Ok(())) => Ok(Self {
                command_tx,
                next_request_id: AtomicU64::new(1),
                closed,
                owner: Some(owner),
                lock: Some(lock),
            }),
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
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.command_tx
            .send(Command::Send {
                method: method.into(),
                params,
                reply: reply_tx,
            })
            .map_err(|_| anyhow!("device disconnected"))?;
        await_send_reply(reply_rx, DEFAULT_REQUEST_TIMEOUT)
    }

    pub fn request(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value> {
        self.ensure_open()?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        self.command_tx
            .send(Command::Request {
                id,
                method: method.into(),
                params,
                deadline: Instant::now() + timeout,
                reply: reply_tx,
            })
            .map_err(|_| anyhow!("device disconnected"))?;
        reply_rx
            .recv_timeout(timeout + Duration::from_millis(100))
            .map_err(|_| anyhow!("request {id} timed out"))?
            .map_err(|error| anyhow!(error))
    }

    pub fn close(&mut self) -> Result<()> {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let _ = self.command_tx.send(Command::Close);
        }
        let joined = self
            .owner
            .take()
            .map(|owner| owner.join())
            .transpose()
            .map_err(|_| anyhow!("Micro HID owner thread panicked"));
        self.lock.take();
        joined?;
        Ok(())
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            bail!("device disconnected")
        }
        Ok(())
    }
}

fn await_send_reply(
    reply_rx: Receiver<std::result::Result<(), String>>,
    timeout: Duration,
) -> Result<()> {
    reply_rx
        .recv_timeout(timeout)
        .map_err(|_| anyhow!("device write timed out"))?
        .map_err(|error| anyhow!(error))
}

struct DeviceLock(File);

impl DeviceLock {
    fn acquire() -> Result<Self> {
        let uid = unsafe { libc::getuid() };
        Self::acquire_at(&std::env::temp_dir().join(format!("herdr-micro-{uid}.lock")))
    }

    fn acquire_at(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        // SAFETY: flock only operates on this live file descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                bail!("another herdr-micro process owns the Codex Micro")
            }
            return Err(error.into());
        }
        Ok(Self(file))
    }
}

impl Drop for DeviceLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor remains valid until this destructor returns.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
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
    ready_tx: SyncSender<std::result::Result<(), String>>,
    closed: Arc<AtomicBool>,
) {
    let result = (|| {
        let mut owner = Owner::open(Arc::new(Mutex::new(command_rx)), event_tx, closed)?;
        if ready_tx.send(Ok(())).is_err() {
            owner.teardown(true);
            bail!("opener dropped")
        }
        owner.run();
        Ok(())
    })();
    if let Err(error) = result {
        let _ = ready_tx.send(Err(error.to_string()));
    }
}

struct Owner {
    manager: objc2_core_foundation::CFRetained<IOHIDManager>,
    device: objc2_core_foundation::CFRetained<IOHIDDevice>,
    transport: Transport,
    run_loop: objc2_core_foundation::CFRetained<CFRunLoop>,
    run_loop_mode: &'static CFString,
    input_buffer: Box<[u8; REPORT_SIZE]>,
    #[allow(dead_code)] // Kept pinned solely for the foreign callback pointer.
    context: Pin<Box<CallbackContext>>,
    callback_rx: Receiver<CallbackEvent>,
    command_rx: Arc<Mutex<Receiver<Command>>>,
    event_tx: Sender<DeviceEvent>,
    reassembler: Reassembler,
    pending: HashMap<u64, Pending>,
    closed: Arc<AtomicBool>,
}

impl Owner {
    fn open(
        command_rx: Arc<Mutex<Receiver<Command>>>,
        event_tx: Sender<DeviceEvent>,
        closed: Arc<AtomicBool>,
    ) -> Result<Self> {
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
            ) {
                Ok(mut owner) => match owner.handshake() {
                    Ok(()) => return Ok(owner),
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
        let context = Box::pin(CallbackContext { callback_tx });
        let mut input_buffer = Box::new([0; REPORT_SIZE]);
        let buffer = NonNull::from(input_buffer.as_mut()).cast::<u8>();
        let context_ptr = context.as_ref().get_ref() as *const CallbackContext as *mut c_void;
        // SAFETY: `input_buffer` and pinned `context` outlive registration;
        // teardown unregisters before either allocation is dropped.
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
            command_rx,
            event_tx,
            reassembler: Reassembler::default(),
            pending: HashMap::new(),
            closed,
        })
    }

    fn handshake(&mut self) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let deadline = Instant::now() + DEFAULT_REQUEST_TIMEOUT;
        self.start_request(
            0,
            "device.status".into(),
            None,
            Instant::now() + DEFAULT_REQUEST_TIMEOUT,
            reply_tx,
        )?;
        while Instant::now() < deadline {
            self.pump();
            match reply_rx.try_recv() {
                Ok(Ok(_)) => return Ok(()),
                Ok(Err(error)) => bail!(error),
                Err(TryRecvError::Disconnected) => bail!("device.status timed out"),
                Err(TryRecvError::Empty) => {}
            }
        }
        bail!("device.status timed out")
    }

    fn run(&mut self) {
        loop {
            self.pump();
            let command = match self.command_rx.lock() {
                Ok(receiver) => receiver.try_recv(),
                Err(_) => break,
            };
            match command {
                Ok(Command::Close) | Err(TryRecvError::Disconnected) => break,
                Ok(Command::Send {
                    method,
                    params,
                    reply,
                }) => {
                    let _ = reply.send(
                        self.write(method, params, None)
                            .map_err(|error| error.to_string()),
                    );
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
            if self.closed.load(Ordering::Acquire) {
                break;
            }
        }
        self.teardown(true);
    }

    fn pump(&mut self) {
        // Running a short turn lets CoreFoundation dispatch HID callbacks while
        // still giving channel commands and request timeouts predictable latency.
        let _ = CFRunLoop::run_in_mode(Some(self.run_loop_mode), 0.01, true);
        while let Ok(event) = self.callback_rx.try_recv() {
            match event {
                CallbackEvent::Report(report) => self.handle_report(&report),
                CallbackEvent::Removed => {
                    self.closed.store(true, Ordering::Release);
                    let _ = self.event_tx.send(DeviceEvent::Disconnected);
                    self.fail_all("Codex Micro disconnected");
                }
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
        if let Err(error) = self.write(method, params, Some(id)) {
            self.fail_pending(id, error.to_string());
            return Err(error);
        }
        Ok(())
    }

    fn write(&self, method: String, params: Option<Value>, id: Option<u64>) -> Result<()> {
        for mut report in encode_message(&method, params.as_ref(), id)? {
            let wire = output_wire(&mut report, self.transport)?;
            let pointer = NonNull::from(&mut *wire).cast::<u8>();
            // SAFETY: `wire` is a live mutable subslice for the duration of the
            // synchronous IOKit call, and its transport-specific size is exact.
            let status = unsafe {
                self.device.set_report(
                    IOHIDReportType::Output,
                    REPORT_ID as isize,
                    pointer,
                    wire.len() as isize,
                )
            };
            if status != kIOReturnSuccess {
                bail!("IOHIDDeviceSetReport failed: 0x{:08X}", status as u32)
            }
        }
        Ok(())
    }

    fn handle_report(&mut self, report: &[u8]) {
        for envelope in self.reassembler.push(report) {
            let Ok(envelope) = envelope else { continue };
            if let Some(id) = envelope.get("id").and_then(Value::as_u64) {
                if envelope.get("result").is_some() || envelope.get("error").is_some() {
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

    fn teardown(&mut self, close_manager: bool) {
        self.closed.store(true, Ordering::Release);
        self.fail_all("device disconnected");
        // SAFETY: This is the same owner thread that registered and scheduled
        // the callbacks. Clearing them precedes unscheduling, close, and drop.
        unsafe {
            self.device.register_input_report_callback(
                NonNull::from(self.input_buffer.as_mut()).cast(),
                REPORT_SIZE as isize,
                None,
                std::ptr::null_mut(),
            );
            self.device
                .register_removal_callback(None, std::ptr::null_mut());
            self.device
                .unschedule_from_run_loop(&self.run_loop, self.run_loop_mode);
        }
        let _ = self.device.close(0);
        if close_manager {
            let _ = self.manager.close(0);
        }
        // `context` drops only after IOKit no longer has a callback registration.
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
    if context.is_null()
        || result != kIOReturnSuccess
        || report_type != IOHIDReportType::Input
        || report_id != REPORT_ID as u32
        || length <= 0
    {
        return;
    }
    let length = usize::try_from(length).unwrap_or(0).min(REPORT_SIZE);
    // SAFETY: IOKit guarantees `report` points to `length` received bytes for
    // this callback; `context` stays pinned until callbacks are unregistered.
    let context = unsafe { &*(context.cast::<CallbackContext>()) };
    let bytes = unsafe { std::slice::from_raw_parts(report.as_ptr(), length) }.to_vec();
    let _ = context.callback_tx.send(CallbackEvent::Report(bytes));
}

unsafe extern "C-unwind" fn removal_callback(
    context: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: Registration keeps this pinned context alive until unregistered.
    let context = unsafe { &*(context.cast::<CallbackContext>()) };
    let _ = context.callback_tx.send(CallbackEvent::Removed);
}

struct DeviceCandidate {
    device: objc2_core_foundation::CFRetained<IOHIDDevice>,
    transport: Transport,
    location: Option<i64>,
    serial: String,
}

impl DeviceCandidate {
    fn sort_key(&self) -> (u8, Option<i64>, &str) {
        candidate_key(self.transport, self.location, &self.serial)
    }
}

fn candidate_key(
    transport: Transport,
    location: Option<i64>,
    serial: &str,
) -> (u8, Option<i64>, &str) {
    (transport_rank(transport), location, serial)
}

fn transport_rank(transport: Transport) -> u8 {
    match transport {
        Transport::Usb => 0,
        Transport::BluetoothLowEnergy => 1,
        Transport::Other => 2,
    }
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
    candidates.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
    if candidates
        .windows(2)
        .any(|pair| pair[0].sort_key() == pair[1].sort_key())
    {
        bail!("multiple Codex Micro devices have the same stable identity")
    }
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

fn output_wire(report: &mut [u8; REPORT_SIZE], transport: Transport) -> Result<&mut [u8]> {
    if report[0] != REPORT_ID {
        bail!("invalid Micro report ID")
    }
    Ok(if transport == Transport::Usb {
        &mut report[1..]
    } else {
        report
    })
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
        let usb = output_wire(&mut report, Transport::Usb).unwrap();
        assert_eq!(usb.len(), 63);
        assert_eq!(usb[0], 2);
        assert_eq!(
            output_wire(&mut report, Transport::BluetoothLowEnergy)
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
    fn request_write_error_reaches_caller() {
        let (_reply_tx, reply_rx) = mpsc::sync_channel(1);
        assert!(await_send_reply(reply_rx, Duration::from_millis(1)).is_err());
    }

    #[test]
    fn lock_excludes_another_micro_owner() {
        let path =
            std::env::temp_dir().join(format!("herdr-micro-lock-test-{}", std::process::id()));
        let first = DeviceLock::acquire_at(&path).unwrap();
        assert!(DeviceLock::acquire_at(&path).is_err());
        drop(first);
        assert!(DeviceLock::acquire_at(&path).is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn close_joins_an_owner_after_removal() {
        let path =
            std::env::temp_dir().join(format!("herdr-micro-close-test-{}", std::process::id()));
        let (command_tx, _command_rx) = mpsc::channel();
        let joined = Arc::new(AtomicBool::new(false));
        let owner_joined = Arc::clone(&joined);
        let mut device = MicroDevice {
            command_tx,
            next_request_id: AtomicU64::new(1),
            closed: Arc::new(AtomicBool::new(true)),
            owner: Some(thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                owner_joined.store(true, Ordering::Release);
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
    fn candidate_order_prefers_usb_then_stable_identity() {
        assert!(
            candidate_key(Transport::Usb, Some(2), "b")
                < candidate_key(Transport::BluetoothLowEnergy, Some(1), "a")
        );
        assert!(
            candidate_key(Transport::Usb, Some(1), "b")
                < candidate_key(Transport::Usb, Some(2), "a")
        );
        assert!(
            candidate_key(Transport::Usb, Some(1), "a")
                < candidate_key(Transport::Usb, Some(1), "b")
        );
    }

    #[test]
    fn input_monitoring_access_is_classified() {
        assert_eq!(
            classify_input_monitoring(IOHIDAccessType::Granted),
            InputMonitoringAccess::Granted
        );
        assert_eq!(
            classify_input_monitoring(IOHIDAccessType::Denied),
            InputMonitoringAccess::Denied
        );
        assert_eq!(
            classify_input_monitoring(IOHIDAccessType::Unknown),
            InputMonitoringAccess::Unknown
        );
    }
}
