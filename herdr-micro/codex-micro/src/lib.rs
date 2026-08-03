//! Codex Micro HID transport, wire framing, raw events, and keymap I/O.
//! Application routing and behavior policy belong in the consuming crate.

pub mod device;
pub mod keymap;
pub mod wire;

pub use device::{
    input_monitoring_access, DeviceEvent, InputMonitoringAccess, MicroDevice, Transport,
    DEFAULT_REQUEST_TIMEOUT, MICRO_PRODUCT_ID, MICRO_VENDOR_ID,
};
