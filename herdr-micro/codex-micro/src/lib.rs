//! Codex Micro USB transport, HID framing, raw events, and keymap I/O.
//! Application routing and behavior policy belong in the consuming crate.

mod device;
pub mod keymap;
mod wire;

pub use device::{DEFAULT_REQUEST_TIMEOUT, DeviceEvent, MicroDevice};
