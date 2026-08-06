//! Codex Micro USB transport, HID framing, raw events, and keymap I/O.
//! Application routing and behavior policy belong in the consuming crate.

pub mod device;
pub mod keymap;
pub mod wire;

pub use device::{
    DeviceEvent, MicroDevice, DEFAULT_REQUEST_TIMEOUT, DEVICE_OPEN_TIMEOUT, MICRO_PRODUCT_ID,
    MICRO_VENDOR_ID,
};
