//! Direct USB IOKit transport for the Work Louder Micro.
//!
//! One owner thread owns every native handle. The public handle only sends
//! commands and waits for replies, so callbacks never outlive their storage.

use std::{
    collections::HashMap,
    ffi::{CStr, c_void},
    pin::Pin,
    ptr::{self, NonNull},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use objc2_core_foundation::{
    CFDictionary, CFNumber, CFRetained, CFRunLoop, CFRunLoopSource, CFString, CFUUID,
    kCFRunLoopDefaultMode,
};
use objc2_io_kit::{
    IOCFPlugInInterface, IOCreatePlugInInterfaceForService, IOIteratorNext, IOObjectRelease,
    IORegistryEntryCreateCFProperty, IOServiceGetMatchingServices, IOServiceMatching,
    IOUSBDevRequestTO, IOUSBDeviceInterface500, IOUSBFindInterfaceRequest,
    IOUSBInterfaceInterface197, USBReEnumerateOptions, io_object_t, io_service_t,
    kIOMainPortDefault, kIOReturnExclusiveAccess, kIOReturnSuccess, kIOUSBFindInterfaceDontCare,
    kUSBIn, kUSBInterrupt, kUSBProductID, kUSBVendorID,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::wire::{REPORT_ID, REPORT_SIZE, Reassembler, encode_message};

const MICRO_VENDOR_ID: i32 = 0x303A;
const MICRO_PRODUCT_ID: i32 = 0x8360;
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const DEVICE_OPEN_TIMEOUT: Duration = Duration::from_secs(7);
const INTERFACE_NUMBER: i64 = 0;
const INTERRUPT_ENDPOINT: u8 = 0x81;
const CONTROL_TIMEOUT_MS: u32 = 2_000;
const HID_SET_REPORT_REQUEST_TYPE: u8 = 0x21; // host-to-device, class, interface
const HID_SET_REPORT: u8 = 0x09;
const HID_OUTPUT_REPORT: u16 = 2;
static NATIVE_WATCHDOG_USERS: Mutex<usize> = Mutex::new(0);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DeviceEvent {
    Key { key: String, action: i64 },
    Joystick { angle: f64, distance: f64 },
    Disconnected { error: String },
}

enum CallbackEvent {
    ReadComplete { result: i32, length: usize },
}

/// Pinned until the async source is removed and the interface is closed.
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
    terminal_error: Arc<OnceLock<String>>,
    owner: Option<JoinHandle<Result<()>>>,
}

impl MicroDevice {
    pub const NATIVE_WATCHDOG_TIMEOUT: Duration = DEVICE_OPEN_TIMEOUT
        .saturating_add(DEFAULT_REQUEST_TIMEOUT)
        .saturating_add(Duration::from_secs(1));

    /// Exclusively captures the USB Micro and completes `device.status`.
    pub fn open_exclusive(event_tx: Sender<DeviceEvent>) -> Result<Self> {
        let _watchdog = NativeWatchdog::start();
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let terminal_error = Arc::new(OnceLock::new());
        let owner_closed = Arc::clone(&closed);
        let owner_terminal_error = Arc::clone(&terminal_error);
        let owner_event_tx = event_tx.clone();
        let owner = thread::Builder::new()
            .name("codex-micro-usb".into())
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
                match result {
                    Ok(result) => {
                        if let Err(error) = &result {
                            let _ = owner_event_tx.send(DeviceEvent::Disconnected {
                                error: error.to_string(),
                            });
                        }
                        result
                    }
                    Err(_) => {
                        let error = "Micro USB owner thread panicked";
                        let _ = owner_event_tx.send(DeviceEvent::Disconnected {
                            error: error.into(),
                        });
                        Err(anyhow!(error))
                    }
                }
            })?;

        match ready_rx.recv_timeout(DEVICE_OPEN_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                command_tx,
                next_request_id: AtomicU64::new(1),
                closed,
                terminal_error,
                owner: Some(owner),
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
            .map_err(|_| self.disconnected_error())?;
        await_send_reply(reply_rx, DEFAULT_REQUEST_TIMEOUT, &self.terminal_error)
    }

    pub fn request(&self, method: impl Into<String>, params: Option<Value>) -> Result<Value> {
        self.ensure_open()?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        self.command_tx
            .send(Command::Request {
                id,
                method: method.into(),
                params,
                deadline: Instant::now() + DEFAULT_REQUEST_TIMEOUT,
                reply: reply_tx,
            })
            .map_err(|_| self.disconnected_error())?;
        reply_rx
            .recv_timeout(DEFAULT_REQUEST_TIMEOUT + Duration::from_millis(100))
            .map_err(|_| {
                self.terminal_error
                    .get()
                    .cloned()
                    .map(anyhow::Error::msg)
                    .unwrap_or_else(|| anyhow!("request {id} timed out"))
            })?
            .map_err(|error| anyhow!(error))
    }

    pub fn close(&mut self) -> Result<()> {
        let _watchdog = self.owner.as_ref().map(|_| NativeWatchdog::start());
        if !self.closed.swap(true, Ordering::AcqRel) {
            let _ = self.command_tx.send(Command::Close);
        }
        match self.owner.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(anyhow!("Micro USB owner thread panicked")),
            None => Ok(()),
        }
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

fn await_send_reply(
    reply_rx: Receiver<std::result::Result<(), String>>,
    timeout: Duration,
    terminal_error: &OnceLock<String>,
) -> Result<()> {
    reply_rx
        .recv_timeout(timeout)
        .map_err(|_| {
            terminal_error
                .get()
                .cloned()
                .map(anyhow::Error::msg)
                .unwrap_or_else(|| anyhow!("device write timed out"))
        })?
        .map_err(|error| anyhow!(error))
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
    terminal_error: Arc<OnceLock<String>>,
) -> Result<()> {
    let result = (|| {
        let mut owner = Owner::open(command_rx, event_tx, closed, terminal_error)?;
        if ready_tx.send(Ok(())).is_err() {
            owner.teardown()?;
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
    device: UsbDevice,
    interface: UsbInterface,
    _device_service: IoObject,
    _interface_service: IoObject,
    pipe: u8,
    run_loop: CFRetained<CFRunLoop>,
    run_loop_mode: &'static CFString,
    async_source: CFRetained<CFRunLoopSource>,
    input_buffer: Box<[u8; REPORT_SIZE]>,
    context: Option<Pin<Box<CallbackContext>>>,
    callback_rx: Receiver<CallbackEvent>,
    command_rx: Receiver<Command>,
    event_tx: Sender<DeviceEvent>,
    reassembler: Reassembler,
    pending: HashMap<u64, Pending>,
    closed: Arc<AtomicBool>,
    read_pending: bool,
    torn_down: bool,
    terminal_error: Arc<OnceLock<String>>,
    // Last so a Drop-armed alarm covers every native field destructor.
    watchdog: Option<NativeWatchdog>,
}

impl Owner {
    fn open(
        command_rx: Receiver<Command>,
        event_tx: Sender<DeviceEvent>,
        closed: Arc<AtomicBool>,
        terminal_error: Arc<OnceLock<String>>,
    ) -> Result<Self> {
        let device_service = find_micro_device()?;
        let mut device = UsbDevice::new(device_service.0)?;
        device.verify_identity()?;
        if let Err(error) = device.capture() {
            return Err(restore_after_open_error(&mut device, error));
        }

        let setup = (|| {
            let interface_service = wait_for_interface_zero(&device)?;
            let mut interface = UsbInterface::new(interface_service.0)?;
            if interface.identity()? != (INTERFACE_NUMBER as u8, 0) {
                bail!("IOKit USB interface identity changed during open")
            }
            interface.open()?;
            let pipe = interface.interrupt_in_pipe()?;

            let run_loop =
                CFRunLoop::current().ok_or_else(|| anyhow!("no Core Foundation run loop"))?;
            // Supplied by CoreFoundation for the process lifetime.
            let run_loop_mode = unsafe { kCFRunLoopDefaultMode }
                .ok_or_else(|| anyhow!("no Core Foundation default run loop mode"))?;
            let async_source = interface.create_async_source()?;
            run_loop.add_source(Some(&async_source), Some(run_loop_mode));
            Ok((
                interface_service,
                interface,
                pipe,
                run_loop,
                run_loop_mode,
                async_source,
            ))
        })();
        let (interface_service, interface, pipe, run_loop, run_loop_mode, async_source) =
            match setup {
                Ok(setup) => setup,
                Err(error) => return Err(restore_after_open_error(&mut device, error)),
            };

        let (callback_tx, callback_rx) = mpsc::channel();
        let context = Box::pin(CallbackContext { callback_tx });
        let mut owner = Self {
            device,
            interface,
            _device_service: device_service,
            _interface_service: interface_service,
            pipe,
            run_loop,
            run_loop_mode,
            async_source,
            input_buffer: Box::new([0; REPORT_SIZE]),
            context: Some(context),
            callback_rx,
            command_rx,
            event_tx,
            reassembler: Reassembler::default(),
            pending: HashMap::new(),
            closed,
            read_pending: false,
            torn_down: false,
            terminal_error,
            watchdog: None,
        };
        if let Err(error) = owner.submit_read() {
            owner
                .teardown()
                .with_context(|| format!("restore USB device after read setup failed: {error}"))?;
            return Err(error);
        }
        if let Err(error) = owner.handshake() {
            owner
                .teardown()
                .with_context(|| format!("restore USB device after handshake failed: {error}"))?;
            return Err(error);
        }
        Ok(owner)
    }

    fn handshake(&mut self) -> Result<()> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let deadline = Instant::now() + DEFAULT_REQUEST_TIMEOUT;
        self.start_request(0, "device.status".into(), None, deadline, reply_tx)?;
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

    fn run(mut self) -> Result<()> {
        loop {
            self.pump();
            if self.closed.load(Ordering::Acquire) {
                break;
            }
            let command = self.command_rx.try_recv();
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
        }
        self.watchdog = Some(NativeWatchdog::start());
        let terminal_error = self.terminal_error.get().cloned();
        let result = finish_owner(terminal_error, self.teardown());
        drop(self);
        result
    }

    fn pump(&mut self) {
        let _ = CFRunLoop::run_in_mode(Some(self.run_loop_mode), 0.01, true);
        while let Ok(CallbackEvent::ReadComplete { result, length }) = self.callback_rx.try_recv() {
            self.read_pending = false;
            if result != kIOReturnSuccess {
                self.disconnect(format!("interrupt read failed: 0x{:08X}", result as u32));
                continue;
            }
            let length = length.min(REPORT_SIZE);
            if length > 0 {
                let bytes = self.input_buffer[..length].to_vec();
                self.handle_report(&bytes);
            }
            if !self.closed.load(Ordering::Acquire)
                && let Err(error) = self.submit_read()
            {
                self.disconnect(error.to_string());
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

    fn submit_read(&mut self) -> Result<()> {
        let context = self
            .context
            .as_ref()
            .expect("callback context is live before teardown")
            .as_ref()
            .get_ref() as *const CallbackContext as *mut c_void;
        self.interface
            .read_async(self.pipe, self.input_buffer.as_mut_ptr().cast(), context)?;
        self.read_pending = true;
        Ok(())
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

    fn write(&mut self, method: String, params: Option<Value>, id: Option<u64>) -> Result<()> {
        for mut report in encode_message(&method, params.as_ref(), id)? {
            let payload = output_wire(&mut report)?;
            if let Err(error) = self.device.set_output_report(payload) {
                self.disconnect(error.to_string());
                return Err(error);
            }
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

    fn disconnect(&mut self, error: String) {
        let error = self.terminal_error.get_or_init(|| error).clone();
        self.closed.store(true, Ordering::Release);
        self.fail_all(&error);
        fail_queued(&self.command_rx, &error);
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

    fn teardown(&mut self) -> Result<()> {
        if self.torn_down {
            return Ok(());
        }
        self.closed.store(true, Ordering::Release);
        let error = self
            .terminal_error
            .get()
            .map(String::as_str)
            .unwrap_or("device disconnected")
            .to_owned();
        self.fail_all(&error);
        fail_queued(&self.command_rx, &error);
        let mut errors = Vec::new();
        if self.read_pending
            && let Err(error) = self.interface.abort(self.pipe)
        {
            errors.push(error.to_string());
        }
        let deadline = Instant::now() + DEFAULT_REQUEST_TIMEOUT;
        while self.read_pending && Instant::now() < deadline {
            let _ = CFRunLoop::run_in_mode(Some(self.run_loop_mode), 0.01, true);
            while let Ok(CallbackEvent::ReadComplete { .. }) = self.callback_rx.try_recv() {
                self.read_pending = false;
            }
        }
        if self.read_pending {
            errors.push("interrupt read did not finish during teardown".into());
            // IOKit still owns these raw pointers; keep them alive until this short-lived
            // helper exits rather than risking a late callback or DMA into freed memory.
            if let Some(context) = self.context.take() {
                std::mem::forget(context);
            }
            let buffer = std::mem::replace(&mut self.input_buffer, Box::new([0; REPORT_SIZE]));
            std::mem::forget(buffer);
        }
        self.run_loop
            .remove_source(Some(&self.async_source), Some(self.run_loop_mode));
        self.async_source.invalidate();
        self.interface.release();
        if let Err(error) = self.device.restore() {
            errors.push(error.to_string());
        }
        self.device.close();
        self.device.release();
        self.torn_down = true;
        if errors.is_empty() {
            Ok(())
        } else {
            bail!(errors.join("; "))
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if self.watchdog.is_none() {
            self.watchdog = Some(NativeWatchdog::start());
        }
        let _ = self.teardown();
    }
}

struct NativeWatchdog;

impl NativeWatchdog {
    fn start() -> Self {
        let mut users = NATIVE_WATCHDOG_USERS.lock().unwrap();
        if *users == 0 {
            // SAFETY: the privileged helper installs SIGALRM with its fatal default
            // disposition before opening the device.
            unsafe {
                libc::alarm(MicroDevice::NATIVE_WATCHDOG_TIMEOUT.as_secs() as libc::c_uint);
            }
        }
        *users += 1;
        Self
    }
}

impl Drop for NativeWatchdog {
    fn drop(&mut self) {
        let mut users = NATIVE_WATCHDOG_USERS.lock().unwrap();
        *users -= 1;
        if *users == 0 {
            // SAFETY: alarm(0) cancels the process-wide native deadline.
            unsafe {
                libc::alarm(0);
            }
        }
    }
}

unsafe extern "C-unwind" fn read_callback(context: *mut c_void, result: i32, length: *mut c_void) {
    if context.is_null() {
        return;
    }
    // SAFETY: Owner pins this context until after abort, source removal, and close.
    let context = unsafe { &*context.cast::<CallbackContext>() };
    let _ = context.callback_tx.send(CallbackEvent::ReadComplete {
        result,
        length: length as usize,
    });
}

struct IoObject(io_object_t);

impl Drop for IoObject {
    fn drop(&mut self) {
        if self.0 != 0 {
            let _ = IOObjectRelease(self.0);
        }
    }
}

struct PlugIn(NonNull<*mut IOCFPlugInInterface>);

impl PlugIn {
    fn new(service: io_service_t, user_client: [u8; 16]) -> Result<Self> {
        let user_client = uuid(user_client);
        let plugin_id = uuid(IOCF_PLUGIN_INTERFACE_ID);
        let mut plugin = ptr::null_mut();
        let mut score = 0;
        // SAFETY: service is retained, UUIDs are valid, and both out-pointers live.
        let status = unsafe {
            IOCreatePlugInInterfaceForService(
                service,
                Some(&user_client),
                Some(&plugin_id),
                &mut plugin,
                &mut score,
            )
        };
        if status != kIOReturnSuccess {
            bail!(
                "IOCreatePlugInInterfaceForService failed: 0x{:08X}",
                status as u32
            )
        }
        Ok(Self(
            NonNull::new(plugin).ok_or_else(|| anyhow!("IOKit returned a null plug-in"))?,
        ))
    }

    fn query<T>(&self, interface_id: [u8; 16]) -> Result<NonNull<*mut T>> {
        let id = uuid(interface_id);
        let mut output = ptr::null_mut();
        // SAFETY: the plug-in COM pointer is live and output has the requested **T layout.
        let status = unsafe {
            let table = &**self.0.as_ptr();
            let query = table
                .QueryInterface
                .ok_or_else(|| anyhow!("IOKit plug-in has no QueryInterface"))?;
            query(
                self.0.as_ptr().cast(),
                id.uuid_bytes(),
                (&mut output as *mut *mut *mut T).cast(),
            )
        };
        if status != 0 {
            bail!("IOKit QueryInterface failed: 0x{:08X}", status as u32)
        }
        NonNull::new(output).ok_or_else(|| anyhow!("IOKit returned a null COM interface"))
    }
}

impl Drop for PlugIn {
    fn drop(&mut self) {
        // SAFETY: this is the matching Release for the live plug-in reference.
        unsafe {
            if let Some(release) = (**self.0.as_ptr()).Release {
                release(self.0.as_ptr().cast());
            }
        }
    }
}

struct UsbDevice {
    raw: NonNull<*mut IOUSBDeviceInterface500>,
    open: bool,
    captured: bool,
    released: bool,
}

impl UsbDevice {
    fn new(service: io_service_t) -> Result<Self> {
        let plugin = PlugIn::new(service, USB_DEVICE_USER_CLIENT_ID)?;
        Ok(Self {
            raw: plugin.query(USB_DEVICE_INTERFACE_ID_500)?,
            open: false,
            captured: false,
            released: false,
        })
    }

    fn table(&self) -> &IOUSBDeviceInterface500 {
        // SAFETY: QueryInterface returned this COM v500 table and self owns its ref.
        unsafe { &**self.raw.as_ptr() }
    }

    fn this(&self) -> *mut c_void {
        self.raw.as_ptr().cast()
    }

    fn verify_identity(&self) -> Result<()> {
        let mut vendor = 0;
        let mut product = 0;
        let get_vendor = self
            .table()
            .GetDeviceVendor
            .ok_or_else(|| anyhow!("USB device interface has no GetDeviceVendor"))?;
        let get_product = self
            .table()
            .GetDeviceProduct
            .ok_or_else(|| anyhow!("USB device interface has no GetDeviceProduct"))?;
        // SAFETY: COM pointer and both out-pointers are valid.
        let vendor_status = unsafe { get_vendor(self.this(), &mut vendor) };
        let product_status = unsafe { get_product(self.this(), &mut product) };
        if vendor_status != kIOReturnSuccess
            || product_status != kIOReturnSuccess
            || vendor != MICRO_VENDOR_ID as u16
            || product != MICRO_PRODUCT_ID as u16
        {
            bail!("IOKit USB device identity changed during open")
        }
        Ok(())
    }

    fn capture(&mut self) -> Result<()> {
        let open = self
            .table()
            .USBDeviceOpenSeize
            .ok_or_else(|| anyhow!("USB device interface has no USBDeviceOpenSeize"))?;
        // SAFETY: live COM device pointer.
        let status = unsafe { open(self.this()) };
        if status == kIOReturnSuccess {
            self.open = true;
        } else if status != kIOReturnExclusiveAccess as i32 {
            bail!("USBDeviceOpenSeize failed: 0x{:08X}", status as u32)
        }

        // Root capture/release re-enumeration explicitly does not require an open
        // device, so exclusive access here can still be resolved by capture.
        let reenumerate = self
            .table()
            .USBDeviceReEnumerate
            .ok_or_else(|| anyhow!("USB device interface has no USBDeviceReEnumerate"))?;
        // SAFETY: live COM device pointer; capture mask is the documented driver-detach path.
        let status = unsafe {
            reenumerate(
                self.this(),
                USBReEnumerateOptions::ReEnumerateCaptureDeviceMask.0 as u32,
            )
        };
        if status != kIOReturnSuccess {
            bail!("USB device capture failed: 0x{:08X}", status as u32)
        }
        self.captured = true;
        self.open = false;

        // Capture invalidates the previous open while the device re-enumerates.
        let deadline = Instant::now() + DEFAULT_REQUEST_TIMEOUT;
        loop {
            let status = unsafe { open(self.this()) };
            if status == kIOReturnSuccess {
                self.open = true;
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("captured USB device reopen failed: 0x{:08X}", status as u32)
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn restore(&mut self) -> Result<()> {
        if !self.captured {
            return Ok(());
        }
        let reenumerate = self
            .table()
            .USBDeviceReEnumerate
            .ok_or_else(|| anyhow!("USB device interface has no USBDeviceReEnumerate"))?;
        // SAFETY: live device pointer; the release mask returns it to driver matching.
        let status = unsafe {
            reenumerate(
                self.this(),
                USBReEnumerateOptions::ReEnumerateReleaseDeviceMask.0 as u32,
            )
        };
        if status != kIOReturnSuccess {
            bail!("USB device restoration failed: 0x{:08X}", status as u32)
        }
        self.captured = false;
        self.open = false;
        Ok(())
    }

    fn set_output_report(&self, payload: &mut [u8]) -> Result<()> {
        let request = self
            .table()
            .DeviceRequestTO
            .ok_or_else(|| anyhow!("USB device interface has no DeviceRequestTO"))?;
        let mut request_data = IOUSBDevRequestTO {
            bmRequestType: HID_SET_REPORT_REQUEST_TYPE,
            bRequest: HID_SET_REPORT,
            wValue: (HID_OUTPUT_REPORT << 8) | REPORT_ID as u16,
            wIndex: INTERFACE_NUMBER as u16,
            wLength: payload.len() as u16,
            pData: payload.as_mut_ptr().cast(),
            wLenDone: 0,
            noDataTimeout: CONTROL_TIMEOUT_MS,
            completionTimeout: CONTROL_TIMEOUT_MS,
        };
        // SAFETY: payload remains live for this bounded synchronous endpoint-zero request.
        let status = unsafe { request(self.this(), &mut request_data) };
        if status != kIOReturnSuccess {
            bail!("HID SET_REPORT failed: 0x{:08X}", status as u32)
        }
        if request_data.wLenDone != payload.len() as u32 {
            bail!(
                "HID SET_REPORT wrote {} of {} bytes",
                request_data.wLenDone,
                payload.len()
            )
        }
        Ok(())
    }

    fn close(&mut self) {
        if !self.open {
            return;
        }
        if let Some(close) = self.table().USBDeviceClose {
            // SAFETY: live and currently open device pointer.
            let _ = unsafe { close(self.this()) };
        }
        self.open = false;
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        // A failed restore leaves `captured` true. Keep the final COM reference
        // alive so Drop can retry restoration without dereferencing freed memory;
        // if that retry also fails, leaking until helper exit is the safe outcome.
        if self.captured {
            return;
        }
        // SAFETY: matching Release for QueryInterface's device reference.
        unsafe {
            if let Some(release) = self.table().Release {
                release(self.this());
            }
        }
        self.released = true;
    }
}

impl Drop for UsbDevice {
    fn drop(&mut self) {
        let _ = self.restore();
        self.close();
        self.release();
    }
}

struct UsbInterface {
    raw: NonNull<*mut IOUSBInterfaceInterface197>,
    open: bool,
    released: bool,
}

impl UsbInterface {
    fn new(service: io_service_t) -> Result<Self> {
        let plugin = PlugIn::new(service, USB_INTERFACE_USER_CLIENT_ID)?;
        Ok(Self {
            raw: plugin.query(USB_INTERFACE_ID_197)?,
            open: false,
            released: false,
        })
    }

    fn table(&self) -> &IOUSBInterfaceInterface197 {
        // SAFETY: QueryInterface returned this COM v197 table and self owns its ref.
        unsafe { &**self.raw.as_ptr() }
    }

    fn this(&self) -> *mut c_void {
        self.raw.as_ptr().cast()
    }

    fn open(&mut self) -> Result<()> {
        let open = self
            .table()
            .USBInterfaceOpen
            .ok_or_else(|| anyhow!("USB interface has no USBInterfaceOpen"))?;
        // SAFETY: live COM interface pointer.
        let status = unsafe { open(self.this()) };
        if status != kIOReturnSuccess {
            bail!("USBInterfaceOpen failed: 0x{:08X}", status as u32)
        }
        self.open = true;
        Ok(())
    }

    fn identity(&self) -> Result<(u8, u8)> {
        let get_number = self
            .table()
            .GetInterfaceNumber
            .ok_or_else(|| anyhow!("USB interface has no GetInterfaceNumber"))?;
        let get_alternate = self
            .table()
            .GetAlternateSetting
            .ok_or_else(|| anyhow!("USB interface has no GetAlternateSetting"))?;
        let (mut number, mut alternate) = (0, 0);
        // SAFETY: live COM interface pointer and valid out-pointers; open is not required.
        let number_status = unsafe { get_number(self.this(), &mut number) };
        let alternate_status = unsafe { get_alternate(self.this(), &mut alternate) };
        if number_status != kIOReturnSuccess || alternate_status != kIOReturnSuccess {
            bail!("failed to identify USB interface")
        }
        Ok((number, alternate))
    }

    fn interrupt_in_pipe(&self) -> Result<u8> {
        let mut count = 0;
        let get_count = self
            .table()
            .GetNumEndpoints
            .ok_or_else(|| anyhow!("USB interface has no GetNumEndpoints"))?;
        // SAFETY: live open interface and valid out-pointer.
        let status = unsafe { get_count(self.this(), &mut count) };
        if status != kIOReturnSuccess {
            bail!("GetNumEndpoints failed: 0x{:08X}", status as u32)
        }
        let get_properties = self
            .table()
            .GetPipeProperties
            .ok_or_else(|| anyhow!("USB interface has no GetPipeProperties"))?;
        let mut found = None;
        for pipe in 1..=count {
            let (mut direction, mut number, mut transfer, mut packet, mut interval) =
                (0, 0, 0, 0, 0);
            // SAFETY: live open interface and valid property out-pointers.
            let status = unsafe {
                get_properties(
                    self.this(),
                    pipe,
                    &mut direction,
                    &mut number,
                    &mut transfer,
                    &mut packet,
                    &mut interval,
                )
            };
            if status != kIOReturnSuccess
                || endpoint_address(direction, number) != INTERRUPT_ENDPOINT
                || transfer != kUSBInterrupt as u8
                || packet != REPORT_SIZE as u16
            {
                continue;
            }
            if found.replace(pipe).is_some() {
                bail!("multiple 0x81/64-byte interrupt IN pipes found")
            }
        }
        found.ok_or_else(|| anyhow!("0x81/64-byte interrupt IN pipe not found"))
    }

    fn create_async_source(&self) -> Result<CFRetained<CFRunLoopSource>> {
        let create = self
            .table()
            .CreateInterfaceAsyncEventSource
            .ok_or_else(|| anyhow!("USB interface has no async event source"))?;
        let mut source = ptr::null_mut();
        // SAFETY: live interface pointer and valid created-source out-pointer.
        let status = unsafe { create(self.this(), &mut source) };
        if status != kIOReturnSuccess {
            bail!(
                "CreateInterfaceAsyncEventSource failed: 0x{:08X}",
                status as u32
            )
        }
        let source = NonNull::new(source).ok_or_else(|| anyhow!("IOKit returned a null source"))?;
        // SAFETY: CreateInterfaceAsyncEventSource returns a +1 CF object.
        Ok(unsafe { CFRetained::from_raw(source) })
    }

    fn read_async(&self, pipe: u8, buffer: *mut c_void, context: *mut c_void) -> Result<()> {
        let read = self
            .table()
            .ReadPipeAsync
            .ok_or_else(|| anyhow!("USB interface has no ReadPipeAsync"))?;
        // SAFETY: buffer and pinned context remain live until completion or abort.
        let status = unsafe {
            read(
                self.this(),
                pipe,
                buffer,
                REPORT_SIZE as u32,
                Some(read_callback),
                context,
            )
        };
        if status != kIOReturnSuccess {
            bail!("ReadPipeAsync failed: 0x{:08X}", status as u32)
        }
        Ok(())
    }

    fn abort(&self, pipe: u8) -> Result<()> {
        if !self.open {
            bail!("USB interface is not open during abort")
        }
        let abort = self
            .table()
            .AbortPipe
            .ok_or_else(|| anyhow!("USB interface has no AbortPipe"))?;
        // SAFETY: live open interface and discovered pipe reference.
        let status = unsafe { abort(self.this(), pipe) };
        if status != kIOReturnSuccess {
            bail!("AbortPipe failed: 0x{:08X}", status as u32)
        }
        Ok(())
    }

    fn close(&mut self) {
        if !self.open {
            return;
        }
        if let Some(close) = self.table().USBInterfaceClose {
            // SAFETY: live and currently open interface pointer.
            let _ = unsafe { close(self.this()) };
        }
        self.open = false;
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.close();
        // SAFETY: matching Release for QueryInterface's interface reference.
        unsafe {
            if let Some(release) = self.table().Release {
                release(self.this());
            }
        }
        self.released = true;
    }
}

impl Drop for UsbInterface {
    fn drop(&mut self) {
        self.release();
    }
}

fn find_micro_device() -> Result<IoObject> {
    // SAFETY: static bytes are a valid NUL-terminated IOKit class name.
    let matching = unsafe { IOServiceMatching(c"IOUSBHostDevice".as_ptr().cast()) }
        .ok_or_else(|| anyhow!("IOServiceMatching failed"))?;
    // SAFETY: CFMutableDictionary is a CFDictionary subtype with identical ownership.
    let matching = unsafe { CFRetained::cast_unchecked::<CFDictionary>(matching) };
    let mut iterator = 0;
    // SAFETY: the matching dictionary is consumed and iterator is a valid out-pointer.
    let status =
        unsafe { IOServiceGetMatchingServices(kIOMainPortDefault, Some(matching), &mut iterator) };
    if status != kIOReturnSuccess {
        bail!(
            "IOServiceGetMatchingServices failed: 0x{:08X}",
            status as u32
        )
    }
    let iterator = IoObject(iterator);
    let mut found = Vec::new();
    loop {
        let service = IOIteratorNext(iterator.0);
        if service == 0 {
            break;
        }
        let service = IoObject(service);
        if property_number(service.0, kUSBVendorID) == Some(MICRO_VENDOR_ID as i64)
            && property_number(service.0, kUSBProductID) == Some(MICRO_PRODUCT_ID as i64)
        {
            found.push(service);
        }
    }
    match found.len() {
        0 => bail!("Codex Micro USB device not found"),
        1 => Ok(found.pop().expect("one device")),
        count => bail!("multiple Codex Micro USB devices found ({count})"),
    }
}

fn restore_after_open_error(device: &mut UsbDevice, error: anyhow::Error) -> anyhow::Error {
    match device.restore() {
        Ok(()) => error,
        Err(recovery) => anyhow!("{error:#}; USB restoration failed: {recovery:#}"),
    }
}

fn finish_owner(terminal_error: Option<String>, teardown: Result<()>) -> Result<()> {
    match (terminal_error, teardown) {
        (Some(error), Ok(())) => Err(anyhow!(error)),
        (Some(error), Err(teardown)) => Err(anyhow!("{error}; USB teardown failed: {teardown:#}")),
        (None, teardown) => teardown,
    }
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

fn wait_for_interface_zero(device: &UsbDevice) -> Result<IoObject> {
    let deadline = Instant::now() + DEFAULT_REQUEST_TIMEOUT;
    loop {
        match find_interface_zero(device) {
            Ok(interface) => return Ok(interface),
            Err(_) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error).context("wait for captured USB interface 0"),
        }
    }
}

fn find_interface_zero(device: &UsbDevice) -> Result<IoObject> {
    let create = device
        .table()
        .CreateInterfaceIterator
        .ok_or_else(|| anyhow!("USB device has no CreateInterfaceIterator"))?;
    let any = kIOUSBFindInterfaceDontCare as u16;
    let mut request = IOUSBFindInterfaceRequest {
        bInterfaceClass: any,
        bInterfaceSubClass: any,
        bInterfaceProtocol: any,
        bAlternateSetting: any,
    };
    let mut iterator = 0;
    // SAFETY: live captured device, initialized request, and valid iterator out-pointer.
    let status = unsafe { create(device.this(), &mut request, &mut iterator) };
    if status != kIOReturnSuccess {
        bail!("CreateInterfaceIterator failed: 0x{:08X}", status as u32)
    }
    let iterator = IoObject(iterator);
    let mut found = Vec::new();
    loop {
        let service = IOIteratorNext(iterator.0);
        if service == 0 {
            break;
        }
        let service = IoObject(service);
        let is_interface_zero = UsbInterface::new(service.0)
            .and_then(|interface| interface.identity())
            .is_ok_and(|identity| identity == (INTERFACE_NUMBER as u8, 0));
        if is_interface_zero {
            found.push(service);
        }
    }
    match found.len() {
        0 => bail!("Codex Micro USB interface 0 not found"),
        1 => Ok(found.pop().expect("one interface")),
        count => bail!("multiple Codex Micro USB interface-0 services found ({count})"),
    }
}

fn property_number(service: io_service_t, key: &CStr) -> Option<i64> {
    let key = CFString::from_str(key.to_str().ok()?);
    // SAFETY: service is retained, key is a CFString, and no options are used.
    unsafe { IORegistryEntryCreateCFProperty(service, Some(&key), None, 0) }
        .and_then(|value| value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i64))
}

fn endpoint_address(direction: u8, number: u8) -> u8 {
    number | if direction == kUSBIn as u8 { 0x80 } else { 0 }
}

fn output_wire(report: &mut [u8; REPORT_SIZE]) -> Result<&mut [u8]> {
    if report[0] != REPORT_ID {
        bail!("invalid Micro report ID")
    }
    Ok(&mut report[1..])
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

fn uuid(bytes: [u8; 16]) -> CFRetained<CFUUID> {
    let [
        b0,
        b1,
        b2,
        b3,
        b4,
        b5,
        b6,
        b7,
        b8,
        b9,
        b10,
        b11,
        b12,
        b13,
        b14,
        b15,
    ] = bytes;
    CFUUID::constant_uuid_with_bytes(
        None, b0, b1, b2, b3, b4, b5, b6, b7, b8, b9, b10, b11, b12, b13, b14, b15,
    )
    .expect("constant UUID")
}

const IOCF_PLUGIN_INTERFACE_ID: [u8; 16] = [
    0xC2, 0x44, 0xE8, 0x58, 0x10, 0x9C, 0x11, 0xD4, 0x91, 0xD4, 0x00, 0x50, 0xE4, 0xC6, 0x42, 0x6F,
];
const USB_DEVICE_USER_CLIENT_ID: [u8; 16] = [
    0x9D, 0xC7, 0xB7, 0x80, 0x9E, 0xC0, 0x11, 0xD4, 0xA5, 0x4F, 0x00, 0x0A, 0x27, 0x05, 0x28, 0x61,
];
const USB_INTERFACE_USER_CLIENT_ID: [u8; 16] = [
    0x2D, 0x97, 0x86, 0xC6, 0x9E, 0xF3, 0x11, 0xD4, 0xAD, 0x51, 0x00, 0x0A, 0x27, 0x05, 0x28, 0x61,
];
const USB_DEVICE_INTERFACE_ID_500: [u8; 16] = [
    0xA3, 0x3C, 0xF0, 0x47, 0x4B, 0x5B, 0x48, 0xE2, 0xB5, 0x7D, 0x02, 0x07, 0xFC, 0xEA, 0xE1, 0x3B,
];
const USB_INTERFACE_ID_197: [u8; 16] = [
    0xC6, 0x3D, 0x3C, 0x92, 0x08, 0x84, 0x11, 0xD7, 0x96, 0x92, 0x00, 0x03, 0x93, 0x3E, 0x3E, 0x3E,
];

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_io_kit::kUSBOut;
    use serde_json::json;

    #[test]
    fn usb_set_report_omits_only_report_id() {
        let mut report = [0; REPORT_SIZE];
        report[0] = REPORT_ID;
        report[1] = 2;
        let payload = output_wire(&mut report).unwrap();
        assert_eq!(payload.len(), 63);
        assert_eq!(payload[0], 2);
    }

    #[test]
    fn endpoint_address_identifies_interrupt_in_endpoint() {
        assert_eq!(endpoint_address(kUSBIn as u8, 1), INTERRUPT_ENDPOINT);
        assert_ne!(endpoint_address(kUSBOut as u8, 1), INTERRUPT_ENDPOINT);
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
    fn close_joins_an_owner_after_removal() {
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
        };
        device.close().unwrap();
        assert!(joined.load(Ordering::Acquire));
        assert!(device.owner.is_none());
    }

    #[test]
    fn terminal_failure_is_kept_with_teardown_failure() {
        let error = finish_owner(
            Some("HID SET_REPORT failed: 0x12345678".into()),
            Err(anyhow!("USB device restoration failed")),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "HID SET_REPORT failed: 0x12345678; USB teardown failed: USB device restoration failed"
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

    #[test]
    fn late_reply_disconnect_keeps_the_terminal_failure() {
        let terminal_error = OnceLock::new();
        terminal_error.set("write failed".into()).unwrap();
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        drop(reply_tx);

        assert_eq!(
            await_send_reply(reply_rx, Duration::ZERO, &terminal_error)
                .unwrap_err()
                .to_string(),
            "write failed"
        );
    }
}
