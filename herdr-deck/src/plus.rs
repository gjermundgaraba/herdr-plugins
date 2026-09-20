use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use elgato_streamdeck::StreamDeckInput;
use elgato_streamdeck::info::Kind;
use elgato_streamdeck::util::{read_button_states, read_encoder_input, read_lcd_input};

const VENDOR_ID: i32 = 0x0fd9;
const PRODUCT_ID: i32 = 0x0084;
const REPORT_SIZE: usize = 1024;
const IO_SUCCESS: i32 = 0;
const HID_OUTPUT: i32 = 1;
const HID_FEATURE: i32 = 2;
const SEIZE_DEVICE: u32 = 1;
const CF_NUMBER_I32: isize = 3;
const CF_STRING_UTF8: u32 = 0x0800_0100;

type CFRef = *const c_void;
type HIDRef = *mut c_void;
type IOReturn = i32;
type CFIndex = isize;

#[repr(C)]
struct CFRunLoopSourceContext {
    version: CFIndex,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    copy_description: Option<extern "C" fn(*const c_void) -> CFRef>,
    equal: Option<extern "C" fn(*const c_void, *const c_void) -> u8>,
    hash: Option<extern "C" fn(*const c_void) -> usize>,
    schedule: Option<extern "C" fn(*const c_void, HIDRef, CFRef)>,
    cancel: Option<extern "C" fn(*const c_void, HIDRef, CFRef)>,
    perform: extern "C" fn(*const c_void),
}

type ReportCallback =
    unsafe extern "C" fn(*mut c_void, IOReturn, *mut c_void, i32, u32, *mut u8, CFIndex);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: CFRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;

    fn CFRetain(value: CFRef) -> CFRef;
    fn CFRelease(value: CFRef);
    fn CFEqual(left: CFRef, right: CFRef) -> u8;
    fn CFStringCreateWithCString(allocator: CFRef, value: *const c_char, encoding: u32) -> CFRef;
    fn CFNumberCreate(allocator: CFRef, number_type: CFIndex, value: *const c_void) -> CFRef;
    fn CFDictionaryCreate(
        allocator: CFRef,
        keys: *const CFRef,
        values: *const CFRef,
        count: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFRef;
    fn CFSetGetCount(set: CFRef) -> CFIndex;
    fn CFSetGetValues(set: CFRef, values: *mut CFRef);
    fn CFRunLoopGetCurrent() -> HIDRef;
    fn CFRunLoopRunInMode(mode: CFRef, seconds: f64, return_after_source: u8) -> i32;
    fn CFRunLoopSourceCreate(
        allocator: CFRef,
        order: CFIndex,
        context: *mut CFRunLoopSourceContext,
    ) -> CFRef;
    fn CFRunLoopAddSource(run_loop: HIDRef, source: CFRef, mode: CFRef);
    fn CFRunLoopRemoveSource(run_loop: HIDRef, source: CFRef, mode: CFRef);
    fn CFRunLoopSourceSignal(source: CFRef);
    fn CFRunLoopWakeUp(run_loop: HIDRef);
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(allocator: CFRef, options: u32) -> HIDRef;
    fn IOHIDManagerSetDeviceMatching(manager: HIDRef, matching: CFRef);
    fn IOHIDManagerOpen(manager: HIDRef, options: u32) -> IOReturn;
    fn IOHIDManagerClose(manager: HIDRef, options: u32) -> IOReturn;
    fn IOHIDManagerCopyDevices(manager: HIDRef) -> CFRef;
    fn IOHIDDeviceGetProperty(device: HIDRef, key: CFRef) -> CFRef;
    fn IOHIDDeviceOpen(device: HIDRef, options: u32) -> IOReturn;
    fn IOHIDDeviceClose(device: HIDRef, options: u32) -> IOReturn;
    fn IOHIDDeviceScheduleWithRunLoop(device: HIDRef, run_loop: HIDRef, mode: CFRef);
    fn IOHIDDeviceUnscheduleFromRunLoop(device: HIDRef, run_loop: HIDRef, mode: CFRef);
    fn IOHIDDeviceRegisterInputReportCallback(
        device: HIDRef,
        report: *mut u8,
        report_length: CFIndex,
        callback: Option<ReportCallback>,
        context: *mut c_void,
    );
    fn IOHIDDeviceSetReport(
        device: HIDRef,
        report_type: i32,
        report_id: CFIndex,
        report: *const u8,
        report_length: CFIndex,
    ) -> IOReturn;
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

struct AutoreleasePool(*mut c_void);

impl AutoreleasePool {
    fn new() -> Self {
        Self(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        unsafe { objc_autoreleasePoolPop(self.0) };
    }
}

struct OwnedCF(CFRef);

impl OwnedCF {
    fn string(value: &str) -> Result<Self, String> {
        let value = CString::new(value).map_err(|_| "CFString contains NUL".to_owned())?;
        let raw = unsafe { CFStringCreateWithCString(ptr::null(), value.as_ptr(), CF_STRING_UTF8) };
        Self::new(raw, "create CFString")
    }

    fn number(value: i32) -> Result<Self, String> {
        let raw =
            unsafe { CFNumberCreate(ptr::null(), CF_NUMBER_I32, (&value as *const i32).cast()) };
        Self::new(raw, "create CFNumber")
    }

    fn new(raw: CFRef, what: &str) -> Result<Self, String> {
        (!raw.is_null())
            .then_some(Self(raw))
            .ok_or_else(|| format!("{what}: returned null"))
    }
}

impl Drop for OwnedCF {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

struct CallbackState {
    inputs: VecDeque<Vec<u8>>,
    input_error: Option<IOReturn>,
    notified: bool,
}

fn notifier_source(state: &mut CallbackState) -> Result<OwnedCF, String> {
    let mut context = CFRunLoopSourceContext {
        version: 0,
        info: (state as *mut CallbackState).cast(),
        retain: None,
        release: None,
        copy_description: None,
        equal: None,
        hash: None,
        schedule: None,
        cancel: None,
        perform: notifier_callback,
    };
    OwnedCF::new(
        unsafe { CFRunLoopSourceCreate(ptr::null(), 0, &mut context) },
        "create Stream Deck + notifier",
    )
}

#[derive(Default)]
struct NotifierState {
    attached: Option<(CFRef, HIDRef)>,
    pending: bool,
}

// Core Foundation run-loop sources are designed to be signalled across threads. Access to the
// borrowed pointers is serialized so detach cannot race a signal.
unsafe impl Send for NotifierState {}

#[derive(Clone, Default)]
pub struct PlusNotifier(Arc<Mutex<NotifierState>>);

impl PlusNotifier {
    pub fn signal(&self) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some((source, run_loop)) = state.attached else {
            state.pending = true;
            return;
        };
        unsafe {
            CFRunLoopSourceSignal(source);
            CFRunLoopWakeUp(run_loop);
        }
    }

    fn attach(&self, source: CFRef, run_loop: HIDRef) {
        unsafe { CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode) };
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        debug_assert!(state.attached.is_none());
        state.attached = Some((source, run_loop));
        if std::mem::take(&mut state.pending) {
            unsafe {
                CFRunLoopSourceSignal(source);
                CFRunLoopWakeUp(run_loop);
            }
        }
    }

    fn detach(&self) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((source, run_loop)) = state.attached.take() {
            unsafe { CFRunLoopRemoveSource(run_loop, source, kCFRunLoopDefaultMode) };
        }
    }
}

pub struct PlusDevice {
    manager: HIDRef,
    device: HIDRef,
    run_loop: HIDRef,
    input_report: Box<[u8; REPORT_SIZE]>,
    source: OwnedCF,
    state: Box<CallbackState>,
    notifier: PlusNotifier,
}

impl PlusDevice {
    pub fn connect(serial: &str, notifier: PlusNotifier) -> Result<Self, String> {
        let manager = unsafe { IOHIDManagerCreate(ptr::null(), 0) };
        if manager.is_null() {
            return Err("create HID manager: returned null".to_owned());
        }

        let result = Self::connect_with_manager(manager, serial, notifier);
        if result.is_err() {
            unsafe {
                IOHIDManagerClose(manager, 0);
                CFRelease(manager.cast());
            }
        }
        result
    }

    fn connect_with_manager(
        manager: HIDRef,
        serial: &str,
        notifier: PlusNotifier,
    ) -> Result<Self, String> {
        let mut state = Box::new(CallbackState {
            inputs: VecDeque::new(),
            input_error: None,
            notified: false,
        });
        let source = notifier_source(&mut state)?;
        let vendor_key = OwnedCF::string("VendorID")?;
        let product_key = OwnedCF::string("ProductID")?;
        let vendor = OwnedCF::number(VENDOR_ID)?;
        let product = OwnedCF::number(PRODUCT_ID)?;
        let keys = [vendor_key.0, product_key.0];
        let values = [vendor.0, product.0];
        let matching = OwnedCF::new(
            unsafe {
                CFDictionaryCreate(
                    ptr::null(),
                    keys.as_ptr(),
                    values.as_ptr(),
                    keys.len() as CFIndex,
                    ptr::addr_of!(kCFTypeDictionaryKeyCallBacks),
                    ptr::addr_of!(kCFTypeDictionaryValueCallBacks),
                )
            },
            "create HID match dictionary",
        )?;
        unsafe { IOHIDManagerSetDeviceMatching(manager, matching.0) };

        let result = unsafe { IOHIDManagerOpen(manager, 0) };
        if result != IO_SUCCESS {
            return Err(io_error("open HID manager", result));
        }

        let devices = OwnedCF::new(
            unsafe { IOHIDManagerCopyDevices(manager) },
            "find Stream Deck +",
        )?;
        let serial_key = OwnedCF::string("SerialNumber")?;
        let wanted_serial = OwnedCF::string(serial)?;
        let count = unsafe { CFSetGetCount(devices.0) }.max(0) as usize;
        let mut candidates = vec![ptr::null(); count];
        unsafe { CFSetGetValues(devices.0, candidates.as_mut_ptr()) };
        let device = candidates
            .into_iter()
            .find(|candidate| {
                let found =
                    unsafe { IOHIDDeviceGetProperty((*candidate).cast_mut(), serial_key.0) };
                !found.is_null() && unsafe { CFEqual(found, wanted_serial.0) != 0 }
            })
            .ok_or_else(|| format!("Stream Deck + serial {serial} not found"))?;
        let device = unsafe { CFRetain(device).cast_mut() };

        let result = unsafe { IOHIDDeviceOpen(device, SEIZE_DEVICE) };
        if result != IO_SUCCESS {
            unsafe { CFRelease(device.cast()) };
            return Err(format!(
                "{}; stop Herdr Deck and Elgato first",
                io_error("seize Stream Deck +", result)
            ));
        }

        let run_loop = unsafe { CFRunLoopGetCurrent() };
        if run_loop.is_null() {
            unsafe {
                IOHIDDeviceClose(device, 0);
                CFRelease(device.cast());
            }
            return Err("get current CFRunLoop: returned null".to_owned());
        }
        unsafe {
            CFRetain(run_loop.cast());
            IOHIDDeviceScheduleWithRunLoop(device, run_loop, kCFRunLoopDefaultMode);
        }

        let mut connected = Self {
            manager,
            device,
            run_loop,
            input_report: Box::new([0; REPORT_SIZE]),
            source,
            state,
            notifier,
        };
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                connected.device,
                connected.input_report.as_mut_ptr(),
                REPORT_SIZE as CFIndex,
                Some(input_callback),
                (&mut *connected.state as *mut CallbackState).cast(),
            );
        }
        connected
            .notifier
            .attach(connected.source.0, connected.run_loop);
        Ok(connected)
    }

    pub fn read_inputs(&mut self) -> Result<Vec<StreamDeckInput>, String> {
        while self.state.inputs.is_empty()
            && self.state.input_error.is_none()
            && !self.state.notified
        {
            Self::run_loop(Duration::from_secs(1000));
        }
        self.state.notified = false;

        if let Some(error) = self.state.input_error.take() {
            return Err(io_error("read Stream Deck + input", error));
        }
        self.state
            .inputs
            .drain(..)
            .map(|data| parse_input(&data))
            .collect()
    }

    pub fn write_button(&mut self, key: u8, jpeg: &[u8]) -> Result<(), String> {
        if key >= 8 {
            return Err(format!("invalid Stream Deck + key {key}"));
        }
        validate_jpeg(jpeg)?;
        self.write_reports(button_reports(key, jpeg)?)
    }

    pub fn write_lcd(&mut self, jpeg: &[u8]) -> Result<(), String> {
        validate_jpeg(jpeg)?;
        self.write_reports(lcd_reports(jpeg)?)
    }

    pub fn set_brightness(&mut self, percent: u8) -> Result<(), String> {
        let mut report = [0; 32];
        report[..3].copy_from_slice(&[0x03, 0x08, percent.min(100)]);
        let result = unsafe {
            IOHIDDeviceSetReport(
                self.device,
                HID_FEATURE,
                0x03,
                report.as_ptr(),
                report.len() as CFIndex,
            )
        };
        (result == IO_SUCCESS)
            .then_some(())
            .ok_or_else(|| io_error("set Stream Deck + brightness", result))
    }

    fn write_reports(&mut self, reports: Vec<[u8; REPORT_SIZE]>) -> Result<(), String> {
        let _pool = AutoreleasePool::new();
        for report in &reports {
            let result = unsafe {
                IOHIDDeviceSetReport(
                    self.device,
                    HID_OUTPUT,
                    0x02,
                    report.as_ptr(),
                    REPORT_SIZE as CFIndex,
                )
            };
            if result != IO_SUCCESS {
                return Err(io_error("write Stream Deck + report", result));
            }
        }
        Ok(())
    }

    fn run_loop(duration: Duration) {
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, duration.as_secs_f64(), 1);
        }
    }
}

impl Drop for PlusDevice {
    fn drop(&mut self) {
        self.notifier.detach();
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                self.device,
                self.input_report.as_mut_ptr(),
                REPORT_SIZE as CFIndex,
                None,
                ptr::null_mut(),
            );
        }
        unsafe {
            IOHIDDeviceUnscheduleFromRunLoop(self.device, self.run_loop, kCFRunLoopDefaultMode);
            IOHIDDeviceClose(self.device, 0);
            IOHIDManagerClose(self.manager, 0);
            CFRelease(self.device.cast());
            CFRelease(self.manager.cast());
            CFRelease(self.run_loop.cast());
        }
    }
}

extern "C" fn notifier_callback(context: *const c_void) {
    if !context.is_null() {
        unsafe { (*context.cast_mut().cast::<CallbackState>()).notified = true };
    }
}

unsafe extern "C" fn input_callback(
    context: *mut c_void,
    result: IOReturn,
    _sender: *mut c_void,
    _report_type: i32,
    _report_id: u32,
    report: *mut u8,
    report_length: CFIndex,
) {
    if context.is_null() {
        return;
    }
    let state = unsafe { &mut *context.cast::<CallbackState>() };
    if result != IO_SUCCESS {
        state.input_error.get_or_insert(result);
    } else if !report.is_null() && report_length > 0 {
        state.inputs.push_back(
            unsafe { std::slice::from_raw_parts(report, report_length as usize) }.to_vec(),
        );
    }
}

fn parse_input(data: &[u8]) -> Result<StreamDeckInput, String> {
    if data.first() == Some(&0) {
        return Ok(StreamDeckInput::NoData);
    }
    let report_type = *data
        .get(1)
        .ok_or_else(|| "short Stream Deck + input report".to_owned())?;
    let touch_length = if data.get(4) == Some(&0x03) { 14 } else { 10 };
    match report_type {
        0x00 if data.len() >= 12 => Ok(StreamDeckInput::ButtonStateChange(read_button_states(
            &Kind::Plus,
            &data[..12],
        ))),
        0x02 if data.len() >= touch_length => {
            read_lcd_input(data).map_err(|error| error.to_string())
        }
        0x03 if data.len() >= 9 => {
            read_encoder_input(&Kind::Plus, data).map_err(|error| error.to_string())
        }
        0x00 | 0x02 | 0x03 => Err(format!(
            "short Stream Deck + input report: {} bytes",
            data.len()
        )),
        other => Err(format!(
            "unknown Stream Deck + input report type 0x{other:02x}"
        )),
    }
}

fn validate_jpeg(jpeg: &[u8]) -> Result<(), String> {
    if jpeg.len() >= 4 && jpeg.starts_with(&[0xff, 0xd8]) && jpeg.ends_with(&[0xff, 0xd9]) {
        Ok(())
    } else {
        Err("input is not a complete JPEG".to_owned())
    }
}

fn button_reports(key: u8, jpeg: &[u8]) -> Result<Vec<[u8; REPORT_SIZE]>, String> {
    frame_reports(jpeg, 8, |report, page, length, last| {
        report[..8].copy_from_slice(&[
            0x02,
            0x07,
            key,
            last as u8,
            length as u8,
            (length >> 8) as u8,
            page as u8,
            (page >> 8) as u8,
        ]);
    })
}

fn lcd_reports(jpeg: &[u8]) -> Result<Vec<[u8; REPORT_SIZE]>, String> {
    frame_reports(jpeg, 16, |report, page, length, last| {
        report[..16].copy_from_slice(&[
            0x02,
            0x0c,
            0,
            0,
            0,
            0,
            0x20,
            0x03,
            100,
            0,
            last as u8,
            page as u8,
            (page >> 8) as u8,
            length as u8,
            (length >> 8) as u8,
            0,
        ]);
    })
}

fn frame_reports(
    data: &[u8],
    header: usize,
    mut set_header: impl FnMut(&mut [u8; REPORT_SIZE], u16, usize, bool),
) -> Result<Vec<[u8; REPORT_SIZE]>, String> {
    let payload = REPORT_SIZE - header;
    let count = data.len().div_ceil(payload);
    if count > usize::from(u16::MAX) + 1 {
        return Err("JPEG needs too many HID reports".to_owned());
    }
    Ok(data
        .chunks(payload)
        .enumerate()
        .map(|(page, chunk)| {
            let mut report = [0; REPORT_SIZE];
            set_header(&mut report, page as u16, chunk.len(), page + 1 == count);
            report[header..header + chunk.len()].copy_from_slice(chunk);
            report
        })
        .collect())
}

fn io_error(action: &str, result: IOReturn) -> String {
    format!("{action}: 0x{:08x}", result as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifier_preserves_early_and_attached_signals() {
        let notifier = PlusNotifier::default();
        let mut state = CallbackState {
            inputs: VecDeque::new(),
            input_error: None,
            notified: false,
        };
        let source = notifier_source(&mut state).unwrap();
        let run_loop = unsafe { CFRunLoopGetCurrent() };

        notifier.signal();
        notifier.attach(source.0, run_loop);
        PlusDevice::run_loop(Duration::from_secs(1));
        let early_signal_arrived = state.notified;

        state.notified = false;
        notifier.signal();
        PlusDevice::run_loop(Duration::from_secs(1));
        let attached_signal_arrived = state.notified;
        notifier.detach();

        assert!(early_signal_arrived);
        assert!(attached_signal_arrived);
    }

    #[test]
    fn frames_pair_headers_with_their_payload_chunks() {
        let mut jpeg = vec![0x5a; 2030];
        jpeg[..2].copy_from_slice(&[0xff, 0xd8]);
        jpeg[2028..].copy_from_slice(&[0xff, 0xd9]);

        let buttons = button_reports(3, &jpeg).unwrap();
        assert_eq!(buttons.len(), 2);
        assert_eq!(&buttons[0][..8], &[2, 7, 3, 0, 248, 3, 0, 0]);
        assert_eq!(&buttons[1][..8], &[2, 7, 3, 1, 246, 3, 1, 0]);
        assert_eq!(
            [&buttons[0][8..1024], &buttons[1][8..8 + 1014]].concat(),
            jpeg
        );

        let lcd = lcd_reports(&jpeg).unwrap();
        assert_eq!(lcd.len(), 3);
        assert_eq!(
            &lcd[0][..16],
            &[2, 12, 0, 0, 0, 0, 32, 3, 100, 0, 0, 0, 0, 240, 3, 0]
        );
        assert_eq!(&lcd[2][10..15], &[1, 2, 0, 14, 0]);
        assert_eq!(
            [&lcd[0][16..1024], &lcd[1][16..1024], &lcd[2][16..30]].concat(),
            jpeg
        );
    }
}
