//! Small AppKit/CoreGraphics helpers used by the daemon and doctor.
//!
//! AppKit identifies the foreground application; Core Graphics posts
//! explicitly configured key input.

use anyhow::{Result, anyhow, bail};
use objc2::rc::Retained;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation, CGPreflightPostEventAccess};
use serde::Serialize;

use crate::config::Modifier;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Frontmost {
    pub app_name: String,
    pub process: String,
    pub pid: i32,
}

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

pub fn frontmost() -> Result<Frontmost> {
    let app =
        frontmost_application().ok_or_else(|| anyhow!("frontmost application unavailable"))?;
    let process = app
        .bundleIdentifier()
        .map_or_else(String::new, |value| value.to_string());
    let app_name = app
        .localizedName()
        .map_or_else(String::new, |value| value.to_string());
    Ok(Frontmost {
        app_name,
        process,
        pid: app.processIdentifier(),
    })
}

fn frontmost_application() -> Option<Retained<NSRunningApplication>> {
    objc2::rc::autoreleasepool(|_| {
        if let Some(mode) = unsafe { kCFRunLoopDefaultMode } {
            let _ = CFRunLoop::run_in_mode(Some(mode), 0.0, true);
        }
        NSWorkspace::sharedWorkspace().frontmostApplication()
    })
}
