//! Small AppKit/CoreGraphics helpers used by the daemon and doctor.
//!
//! AppKit identifies the foreground application; Core Graphics locates its
//! visible normal window and posts explicitly configured key input.

use anyhow::{Result, anyhow, bail};
use objc2::rc::Retained;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{
    CFDictionary, CFNumber, CFRunLoop, CFString, CFType, CGRect, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode, CGEvent,
    CGEventFlags, CGEventTapLocation, CGGetDisplaysWithRect, CGPreflightPostEventAccess,
    CGRectIntersection, CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo,
    CGWindowListOption, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer, kCGWindowOwnerPID,
};
use objc2_foundation::NSString;
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

pub fn frontmost_pid() -> Result<i32> {
    frontmost_application()
        .map(|app| app.processIdentifier())
        .ok_or_else(|| anyhow!("frontmost application unavailable"))
}

pub fn frontmost_bundle_is(expected: &str) -> bool {
    frontmost_application()
        .and_then(|app| app.bundleIdentifier())
        .is_some_and(|bundle| bundle.to_string() == expected)
}

pub fn frontmost_window_scale() -> Result<f64> {
    let app =
        frontmost_application().ok_or_else(|| anyhow!("frontmost application unavailable"))?;
    let window = normal_window_for(&app).ok_or_else(|| anyhow!("frontmost window unavailable"))?;
    let bounds =
        window_bounds(&window).ok_or_else(|| anyhow!("frontmost window bounds unavailable"))?;
    let mut displays = [0 as CGDirectDisplayID; 8];
    let mut count = 0;
    // SAFETY: Both output pointers reference the stack storage declared above.
    let error = unsafe {
        CGGetDisplaysWithRect(
            bounds,
            displays.len() as u32,
            displays.as_mut_ptr(),
            &mut count,
        )
    };
    if error.0 != 0 || count == 0 {
        bail!("frontmost window display unavailable")
    }
    let display = displays[..count as usize]
        .iter()
        .copied()
        .max_by(|left, right| {
            let left = CGRectIntersection(bounds, CGDisplayBounds(*left));
            let right = CGRectIntersection(bounds, CGDisplayBounds(*right));
            (left.size.width * left.size.height).total_cmp(&(right.size.width * right.size.height))
        })
        .ok_or_else(|| anyhow!("frontmost window display unavailable"))?;
    let mode = CGDisplayCopyDisplayMode(display)
        .ok_or_else(|| anyhow!("frontmost window display mode unavailable"))?;
    let width = CGDisplayMode::width(Some(&mode));
    let pixel_width = CGDisplayMode::pixel_width(Some(&mode));
    if width == 0 || pixel_width == 0 {
        bail!("frontmost window display scale unavailable")
    }
    Ok(pixel_width as f64 / width as f64)
}

pub fn bundle_is_running(bundle_id: &str) -> bool {
    objc2::rc::autoreleasepool(|_| {
        !NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(
            bundle_id,
        ))
        .is_empty()
    })
}

fn normal_window_for(
    app: &NSRunningApplication,
) -> Option<objc2_core_foundation::CFRetained<CFDictionary>> {
    let pid = app.processIdentifier();
    if pid < 0 {
        return None;
    }
    let windows = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        kCGNullWindowID,
    )?;
    // SAFETY: Core Graphics documents this array as CFDictionary window records.
    let windows = unsafe { windows.cast_unchecked::<CFDictionary>() };
    for window in windows.iter() {
        let typed = unsafe { window.cast_unchecked::<CFString, CFType>() };
        if usable_window_for(
            pid,
            number(typed, unsafe { kCGWindowLayer }),
            number(typed, unsafe { kCGWindowOwnerPID }),
        ) {
            return Some(window);
        }
    }
    None
}

fn frontmost_application() -> Option<Retained<NSRunningApplication>> {
    objc2::rc::autoreleasepool(|_| {
        if let Some(mode) = unsafe { kCFRunLoopDefaultMode } {
            let _ = CFRunLoop::run_in_mode(Some(mode), 0.0, true);
        }
        NSWorkspace::sharedWorkspace().frontmostApplication()
    })
}

fn usable_window_for(pid: i32, layer: Option<f64>, owner_pid: Option<f64>) -> bool {
    layer == Some(0.0) && owner_pid == Some(f64::from(pid))
}

fn window_bounds(window: &CFDictionary) -> Option<CGRect> {
    let window = unsafe { window.cast_unchecked::<CFString, CFType>() };
    let value = window.get(unsafe { kCGWindowBounds })?;
    let dictionary = value.downcast_ref::<CFDictionary>()?;
    let mut bounds = CGRect::default();
    // SAFETY: Core Graphics supplied the bounds dictionary and `bounds` is writable.
    unsafe { CGRectMakeWithDictionaryRepresentation(Some(dictionary), &mut bounds) }
        .then_some(bounds)
}

fn number(dictionary: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<f64> {
    dictionary.get(key)?.downcast_ref::<CFNumber>()?.as_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_normal_window_owned_by_the_active_pid_is_usable() {
        assert!(usable_window_for(42, Some(0.0), Some(42.0)));
        assert!(!usable_window_for(42, Some(1.0), Some(42.0)));
        assert!(!usable_window_for(42, Some(0.0), Some(7.0)));
        assert!(!usable_window_for(42, None, Some(42.0)));
    }
}
