//! `key` actions: a tap of a virtual keycode, posted through
//! `herdr_device::macos` like prompts and Enter.

use crate::config::Modifier;
use herdr_device::{
    PostError,
    macos::{self, CGEventFlags, Flags},
};

/// Post a tap (down then up) of a virtual keycode with the given modifiers,
/// added to the flags Core Graphics gives the event.
pub fn post_key(keycode: u16, modifiers: &[Modifier]) -> Result<(), PostError> {
    let flags = modifiers.iter().fold(CGEventFlags(0), |flags, modifier| {
        flags
            | match modifier {
                Modifier::Cmd => CGEventFlags::MaskCommand,
                Modifier::Shift => CGEventFlags::MaskShift,
                Modifier::Alt => CGEventFlags::MaskAlternate,
                Modifier::Ctrl => CGEventFlags::MaskControl,
                Modifier::Fn => CGEventFlags::MaskSecondaryFn,
            }
    });
    macos::press(keycode, Flags::Added(flags))
}
