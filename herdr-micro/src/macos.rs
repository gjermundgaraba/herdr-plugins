//! Core Graphics helpers used by the daemon and doctor.

use crate::config::Modifier;
use anyhow::{Result, anyhow, bail};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation, CGPreflightPostEventAccess};

pub fn post_event_access() -> Result<()> {
    if CGPreflightPostEventAccess() {
        Ok(())
    } else {
        bail!("macOS post-event access is denied")
    }
}

/// Post a tap (down then up) of a virtual keycode with the given modifiers.
pub fn post_key(keycode: u16, modifiers: &[Modifier]) -> Result<()> {
    post_event_access()?;
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
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(None, keycode, down)
            .ok_or_else(|| anyhow!("could not create keycode {keycode} event"))?;
        // Only add to the default flags: CGEvent derives required masks (e.g.
        // secondary-fn on F-keys) that hotkey listeners match on.
        if !flags.is_empty() {
            CGEvent::set_flags(Some(&event), CGEvent::flags(Some(&event)) | flags);
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    Ok(())
}
