//! Codex Micro USB transport, HID framing, raw events, and keymap I/O.
//! Application routing and behavior policy belong in the consuming crate.

pub mod device;
pub mod keymap;
pub mod wire;

pub use device::{
    ACTION_KEYS, DEFAULT_REQUEST_TIMEOUT, DEVICE_OPEN_TIMEOUT, DeviceEvent, MICRO_PRODUCT_ID,
    MICRO_VENDOR_ID, MicroDevice,
};
