use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use elgato_streamdeck::info::Kind;
use elgato_streamdeck::{StreamDeck, StreamDeckInput, new_hidapi};
use hidapi::HidApi;

use crate::plus::{PlusDevice, PlusNotifier};

const INPUT_POLL: Duration = Duration::from_millis(5);
const PEDAL_INPUT_POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfo {
    pub kind: Kind,
    pub model: String,
    pub serial_number: String,
    pub path: String,
}

pub fn list_devices() -> Result<Vec<DeviceInfo>, String> {
    let hid = new_hidapi().map_err(|error| error.to_string())?;
    Ok(collect_devices(&hid))
}

pub struct DeviceScanner(HidApi);

impl DeviceScanner {
    pub fn new() -> Result<Self, String> {
        new_hidapi().map(Self).map_err(|error| error.to_string())
    }

    pub fn scan(&mut self) -> Result<Vec<DeviceInfo>, String> {
        self.0.reset_devices().map_err(|error| error.to_string())?;
        self.0
            .add_devices(0x0fd9, 0)
            .map_err(|error| error.to_string())?;
        Ok(collect_devices(&self.0))
    }
}

fn collect_devices(hid: &HidApi) -> Vec<DeviceInfo> {
    let mut devices: Vec<_> = hid
        .device_list()
        .filter_map(|device| {
            let kind = Kind::from_vid_pid(device.vendor_id(), device.product_id())?;
            let serial_number = device.serial_number()?.to_owned();
            Some(DeviceInfo {
                kind,
                model: model_name(kind).to_owned(),
                serial_number,
                path: device.path().to_string_lossy().into_owned(),
            })
        })
        .collect();
    devices.sort_by(|a, b| {
        (&a.model, &a.serial_number, &a.path).cmp(&(&b.model, &b.serial_number, &b.path))
    });
    devices.dedup_by(|a, b| a.path == b.path);
    devices
}

pub const fn model_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Original => "original",
        Kind::OriginalV2 => "originalv2",
        Kind::Mini => "mini",
        Kind::Xl => "xl",
        Kind::XlV2 => "xl",
        Kind::Mk2 => "original-mk2",
        Kind::Mk2Scissor => "original-mk2-scissor",
        Kind::MiniMk2 | Kind::MiniDiscord => "mini",
        Kind::Neo => "neo",
        Kind::Pedal => "pedal",
        Kind::Plus => "plus",
        Kind::PlusXl => "plus-xl",
        Kind::MiniMk2Module => "6-module",
        Kind::Mk2Module => "15-module",
        Kind::XlV2Module => "32-module",
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputEvent {
    ButtonDown(u8),
    EncoderDown(u8),
    EncoderTwist { index: u8, amount: i8 },
    TouchPress { x: u16, y: u16, long: bool },
    TouchSwipe { from: (u16, u16), to: (u16, u16) },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceEvent {
    Ready(DeviceInfo),
    Input {
        serial_number: String,
        event: InputEvent,
    },
    Failed {
        serial_number: String,
        error: String,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FrameBatch {
    pub keys: BTreeMap<u8, Arc<[u8]>>,
    pub lcd: Option<Arc<[u8]>>,
}

impl FrameBatch {
    fn merge(&mut self, newer: Self) {
        self.keys.extend(newer.keys);
        if newer.lcd.is_some() {
            self.lcd = newer.lcd;
        }
    }
}

pub struct DeviceHandle {
    pending: Arc<Mutex<Option<FrameBatch>>>,
    brightness: Arc<Mutex<Option<u8>>>,
    stopped: Arc<AtomicBool>,
    notifier: Option<PlusNotifier>,
    thread: Option<JoinHandle<()>>,
}

impl DeviceHandle {
    pub fn spawn<F>(info: DeviceInfo, brightness: u8, events: F) -> Self
    where
        F: Fn(DeviceEvent) + Send + 'static,
    {
        let pending = Arc::new(Mutex::new(None));
        let desired_brightness = Arc::new(Mutex::new(Some(brightness)));
        let stopped = Arc::new(AtomicBool::new(false));
        let notifier = (info.kind == Kind::Plus).then(PlusNotifier::default);
        let worker_pending = Arc::clone(&pending);
        let worker_brightness = Arc::clone(&desired_brightness);
        let worker_stopped = Arc::clone(&stopped);
        let worker_notifier = notifier.clone();
        if let Some(notifier) = &notifier {
            notifier.signal();
        }
        let thread = thread::spawn(move || {
            if let Err(error) = run_device(
                &info,
                &events,
                &worker_pending,
                &worker_brightness,
                &worker_stopped,
                worker_notifier.as_ref(),
            ) {
                events(DeviceEvent::Failed {
                    serial_number: info.serial_number.clone(),
                    error,
                });
            }
        });
        Self {
            pending,
            brightness: desired_brightness,
            stopped,
            notifier,
            thread: Some(thread),
        }
    }

    pub fn submit(&self, batch: FrameBatch) {
        {
            let mut slot = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match slot.as_mut() {
                Some(current) => current.merge(batch),
                None => *slot = Some(batch),
            }
        }
        self.notify();
    }

    pub fn set_brightness(&self, brightness: u8) {
        *self
            .brightness
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(brightness);
        self.notify();
    }

    fn stop_inner(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.notify();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn notify(&self) {
        if let Some(notifier) = &self.notifier {
            notifier.signal();
        }
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

fn run_device<F>(
    info: &DeviceInfo,
    events: &F,
    pending: &Mutex<Option<FrameBatch>>,
    desired_brightness: &Mutex<Option<u8>>,
    stopped: &AtomicBool,
    notifier: Option<&PlusNotifier>,
) -> Result<(), String>
where
    F: Fn(DeviceEvent),
{
    if info.kind == Kind::Plus {
        let notifier = notifier.ok_or_else(|| "missing Stream Deck + notifier".to_owned())?;
        return run_plus_device(info, events, pending, desired_brightness, stopped, notifier);
    }

    let hid = new_hidapi().map_err(|error| error.to_string())?;
    let deck = StreamDeck::connect(&hid, info.kind, &info.serial_number)
        .map_err(|error| error.to_string())?;
    events(DeviceEvent::Ready(info.clone()));

    let mut buttons = vec![false; info.kind.key_count() as usize];
    let mut encoders = vec![false; info.kind.encoder_count() as usize];

    while !stopped.load(Ordering::Acquire) {
        let input = deck
            .read_input(Some(if info.kind == Kind::Pedal {
                PEDAL_INPUT_POLL
            } else {
                INPUT_POLL
            }))
            .map_err(|error| error.to_string())?;
        for event in map_input(input, &mut buttons, &mut encoders) {
            events(DeviceEvent::Input {
                serial_number: info.serial_number.clone(),
                event,
            });
        }

        let batch = pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(batch) = batch {
            write_frame(&deck, batch)?;
            if info.kind != Kind::Pedal {
                let brightness = desired_brightness
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                // Best effort: this firmware applies it immediately; a failure must not drop HID.
                if let Some(brightness) = brightness {
                    let _ = deck.set_brightness(brightness);
                }
            }
        }
    }
    Ok(())
}

fn run_plus_device<F>(
    info: &DeviceInfo,
    events: &F,
    pending: &Mutex<Option<FrameBatch>>,
    desired_brightness: &Mutex<Option<u8>>,
    stopped: &AtomicBool,
    notifier: &PlusNotifier,
) -> Result<(), String>
where
    F: Fn(DeviceEvent),
{
    let mut deck = PlusDevice::connect(&info.serial_number, notifier.clone())?;
    events(DeviceEvent::Ready(info.clone()));

    let mut buttons = vec![false; info.kind.key_count() as usize];
    let mut encoders = vec![false; info.kind.encoder_count() as usize];

    while !stopped.load(Ordering::Acquire) {
        for input in deck.read_inputs()? {
            for event in map_input(input, &mut buttons, &mut encoders) {
                events(DeviceEvent::Input {
                    serial_number: info.serial_number.clone(),
                    event,
                });
            }
        }

        if stopped.load(Ordering::Acquire) {
            break;
        }

        let batch = pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(batch) = batch {
            write_plus_frame(&mut deck, batch)?;
        }
        let brightness = desired_brightness
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(brightness) = brightness {
            let _ = deck.set_brightness(brightness);
        }
    }
    Ok(())
}

fn write_frame(deck: &StreamDeck, batch: FrameBatch) -> Result<(), String> {
    for (key, jpeg) in batch.keys {
        deck.write_image(key, &jpeg)
            .map_err(|error| error.to_string())?;
    }
    deck.flush().map_err(|error| error.to_string())?;
    if let Some(jpeg) = batch.lcd {
        deck.write_lcd_fill(&jpeg)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn write_plus_frame(deck: &mut PlusDevice, batch: FrameBatch) -> Result<(), String> {
    for (key, jpeg) in batch.keys {
        deck.write_button(key, &jpeg)?;
    }
    if let Some(jpeg) = batch.lcd {
        deck.write_lcd(&jpeg)?;
    }
    Ok(())
}

fn map_input(
    input: StreamDeckInput,
    buttons: &mut Vec<bool>,
    encoders: &mut Vec<bool>,
) -> Vec<InputEvent> {
    match input {
        StreamDeckInput::ButtonStateChange(next) => {
            let events = down_edges(buttons, &next, InputEvent::ButtonDown);
            *buttons = next;
            events
        }
        StreamDeckInput::EncoderStateChange(next) => {
            let events = down_edges(encoders, &next, InputEvent::EncoderDown);
            *encoders = next;
            events
        }
        StreamDeckInput::EncoderTwist(amounts) => amounts
            .into_iter()
            .enumerate()
            .filter(|(_, amount)| *amount != 0)
            .map(|(index, amount)| InputEvent::EncoderTwist {
                index: index as u8,
                amount,
            })
            .collect(),
        StreamDeckInput::TouchScreenPress(x, y) => {
            vec![InputEvent::TouchPress { x, y, long: false }]
        }
        StreamDeckInput::TouchScreenLongPress(x, y) => {
            vec![InputEvent::TouchPress { x, y, long: true }]
        }
        StreamDeckInput::TouchScreenSwipe(from, to) => {
            vec![InputEvent::TouchSwipe { from, to }]
        }
        StreamDeckInput::NoData => Vec::new(),
    }
}

fn down_edges<T>(previous: &[bool], next: &[bool], make: T) -> Vec<InputEvent>
where
    T: Fn(u8) -> InputEvent,
{
    next.iter()
        .enumerate()
        .filter(|(index, down)| **down && !previous.get(*index).copied().unwrap_or(false))
        .map(|(index, _)| make(index as u8))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_frames_keep_the_latest_value_per_output() {
        let mut frame = FrameBatch {
            keys: BTreeMap::from([(0, Arc::from([1])), (1, Arc::from([1]))]),
            lcd: Some(Arc::from([1])),
        };
        frame.merge(FrameBatch {
            keys: BTreeMap::from([(1, Arc::from([2])), (2, Arc::from([2]))]),
            lcd: None,
        });
        frame.merge(FrameBatch {
            keys: BTreeMap::new(),
            lcd: Some(Arc::from([3])),
        });

        assert_eq!(
            frame.keys,
            BTreeMap::from([
                (0, Arc::from([1])),
                (1, Arc::from([2])),
                (2, Arc::from([2]))
            ])
        );
        assert_eq!(frame.lcd, Some(Arc::from([3])));
    }

    #[test]
    fn state_reports_emit_down_edges_only() {
        let mut buttons = vec![false, false, false];
        let mut encoders = Vec::new();
        assert_eq!(
            map_input(
                StreamDeckInput::ButtonStateChange(vec![true, false, true]),
                &mut buttons,
                &mut encoders,
            ),
            vec![InputEvent::ButtonDown(0), InputEvent::ButtonDown(2)]
        );
        assert!(
            map_input(
                StreamDeckInput::ButtonStateChange(vec![true, false, false]),
                &mut buttons,
                &mut encoders,
            )
            .is_empty()
        );
    }
}
