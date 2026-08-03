//! Small AppKit/CoreGraphics helpers used by the daemon and doctor.
//!
//! This intentionally uses the normal on-screen window list only.  It does
//! not inspect accessibility objects or post global keyboard input.

use std::{ptr::NonNull, thread, time::Duration};

use anyhow::{anyhow, bail, Result};
use objc2::rc::Retained;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::{CFDictionary, CFNumber, CFString, CFType, CGPoint};
use objc2_core_graphics::{
    kCGNullWindowID, kCGWindowBounds, kCGWindowLayer, kCGWindowName, kCGWindowOwnerPID, CGEvent,
    CGEventTapLocation, CGEventType, CGMouseButton, CGPreflightPostEventAccess, CGScrollEventUnit,
    CGWarpMouseCursorPosition, CGWindowListCopyWindowInfo, CGWindowListOption,
};
use serde::Serialize;

const SCROLL_TARGET_UNAVAILABLE: &str = "frontmost scroll target unavailable";

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Frontmost {
    pub app_name: String,
    pub process: String,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

pub fn post_event_access() -> Result<()> {
    if CGPreflightPostEventAccess() {
        Ok(())
    } else {
        bail!("macOS post-event access is denied")
    }
}

pub fn frontmost() -> Result<Frontmost> {
    let (app, window) = frontmost_window().ok_or_else(|| anyhow!("no frontmost application"))?;
    let process = app
        .bundleIdentifier()
        .map_or_else(String::new, |value| value.to_string());
    let app_name = app
        .localizedName()
        .map_or_else(String::new, |value| value.to_string());
    let title = window_title(&window).unwrap_or_default();
    Ok(Frontmost {
        app_name,
        process,
        title,
    })
}

/// Routes line scroll events through the visible normal-layer window after
/// confirming that the expected bundle still owns the foreground.
pub fn scroll(notches: i32, x: f64, y: f64, expected_bundle: &str) -> Result<()> {
    post_event_access()?;
    let (app, window) = frontmost_window().ok_or_else(|| anyhow!(SCROLL_TARGET_UNAVAILABLE))?;
    if app
        .bundleIdentifier()
        .map(|value| value.to_string())
        .as_deref()
        != Some(expected_bundle)
    {
        bail!(SCROLL_TARGET_UNAVAILABLE)
    }
    let bounds = window_bounds(&window).ok_or_else(|| anyhow!(SCROLL_TARGET_UNAVAILABLE))?;
    let location = point_in_bounds(bounds, x, y);
    let original = CGEvent::new(None).map(|event| CGEvent::location(Some(&event)));
    let _ = CGWarpMouseCursorPosition(location);
    if let Some(event) =
        CGEvent::new_mouse_event(None, CGEventType::MouseMoved, location, CGMouseButton::Left)
    {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    let _restore = CursorRestore { original };
    thread::sleep(Duration::from_millis(50));
    for _ in 0..notches.unsigned_abs() {
        let event = CGEvent::new_scroll_wheel_event2(
            None,
            CGScrollEventUnit::Line,
            1,
            if notches > 0 { 1 } else { -1 },
            0,
            0,
        )
        .ok_or_else(|| anyhow!("could not create scroll event"))?;
        CGEvent::set_location(Some(&event), location);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    thread::sleep(Duration::from_millis(50));
    Ok(())
}

struct CursorRestore {
    original: Option<CGPoint>,
}

impl Drop for CursorRestore {
    fn drop(&mut self) {
        let Some(location) = self.original else {
            return;
        };
        let _ = CGWarpMouseCursorPosition(location);
        if let Some(event) =
            CGEvent::new_mouse_event(None, CGEventType::MouseMoved, location, CGMouseButton::Left)
        {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    }
}

fn frontmost_window() -> Option<(
    Retained<NSRunningApplication>,
    objc2_core_foundation::CFRetained<CFDictionary>,
)> {
    let windows = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        kCGNullWindowID,
    )?;
    // SAFETY: Core Graphics documents this array as CFDictionary window records.
    let windows = unsafe { windows.cast_unchecked::<CFDictionary>() };
    // The returned CFArray owns each record; callers only need it during this
    // function. This cannot return a borrowed record, so retain it first.
    for index in 0..windows.len() {
        // SAFETY: index is bounded by `len`, and Core Graphics guarantees a
        // CFDictionary at each position in this window-info array.
        let window = unsafe { windows.get_unchecked(index as isize) };
        let typed = unsafe { window.cast_unchecked::<CFString, CFType>() };
        if number(typed, unsafe { kCGWindowLayer }) == Some(0.0) {
            let Some(pid) = number(typed, unsafe { kCGWindowOwnerPID }) else {
                continue;
            };
            let Some(app) =
                NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32)
            else {
                continue;
            };
            // SAFETY: the CFArray retains this record. Retaining it gives the
            // returned handle independent ownership after the array is dropped.
            let window =
                unsafe { objc2_core_foundation::CFRetained::retain(NonNull::from(window)) };
            return Some((app, window));
        }
    }
    None
}

fn window_title(window: &CFDictionary) -> Option<String> {
    // SAFETY: Core Graphics window dictionaries use CFString keys and values
    // of documented CoreFoundation types.
    let window = unsafe { window.cast_unchecked::<CFString, CFType>() };
    let value = window.get(unsafe { kCGWindowName })?;
    value.downcast_ref::<CFString>().map(ToString::to_string)
}

fn window_bounds(window: &CFDictionary) -> Option<Bounds> {
    // SAFETY: Core Graphics window dictionaries use CFString keys and values
    // of documented CoreFoundation types.
    let window = unsafe { window.cast_unchecked::<CFString, CFType>() };
    let bounds = window.get(unsafe { kCGWindowBounds })?;
    let bounds = bounds.downcast_ref::<CFDictionary>()?;
    // SAFETY: CGRect dictionaries are CFString-to-CFNumber mappings.
    let bounds = unsafe { bounds.cast_unchecked::<CFString, CFType>() };
    Some(Bounds {
        x: number(bounds, &CFString::from_str("X"))?,
        y: number(bounds, &CFString::from_str("Y"))?,
        width: number(bounds, &CFString::from_str("Width"))?,
        height: number(bounds, &CFString::from_str("Height"))?,
    })
}

fn number(dictionary: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<f64> {
    dictionary.get(key)?.downcast_ref::<CFNumber>()?.as_f64()
}

fn point_in_bounds(bounds: Bounds, x: f64, y: f64) -> CGPoint {
    CGPoint {
        x: bounds.x + bounds.width * x,
        y: bounds.y + bounds.height * y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_target_uses_window_bounds() {
        let point = point_in_bounds(
            Bounds {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 200.0,
            },
            0.5,
            0.25,
        );
        assert_eq!(point, CGPoint { x: 60.0, y: 70.0 });
    }
}
