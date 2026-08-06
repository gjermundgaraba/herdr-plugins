//! Small AppKit/CoreGraphics helpers used by the daemon and doctor.
//!
//! AppKit identifies the foreground application; Core Graphics supplies that
//! application's visible normal window and posts explicitly configured input.

use std::{ptr::NonNull, thread, time::Duration};

use anyhow::{anyhow, bail, Result};
use objc2::rc::Retained;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{
    kCFRunLoopDefaultMode, CFDictionary, CFNumber, CFRunLoop, CFString, CFType, CGPoint,
};
use objc2_core_graphics::{
    kCGNullWindowID, kCGWindowBounds, kCGWindowLayer, kCGWindowName, kCGWindowOwnerPID, CGEvent,
    CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton, CGPreflightPostEventAccess,
    CGScrollEventUnit, CGWarpMouseCursorPosition, CGWindowListCopyWindowInfo, CGWindowListOption,
};
use serde::Serialize;

const SCROLL_TARGET_UNAVAILABLE: &str = "frontmost scroll target unavailable";

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Frontmost {
    pub app_name: String,
    pub process: String,
    pub pid: i32,
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

pub fn post_function_key(key: &str, down: bool) -> Result<()> {
    post_event_access()?;
    let keycode = function_key_code(key).ok_or_else(|| anyhow!("unsupported macOS key: {key}"))?;
    let event = CGEvent::new_keyboard_event(None, keycode, down)
        .ok_or_else(|| anyhow!("could not create {key} event"))?;
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

fn function_key_code(key: &str) -> Option<CGKeyCode> {
    Some(match key {
        "F13" => 0x69,
        "F14" => 0x6B,
        "F15" => 0x71,
        "F16" => 0x6A,
        "F17" => 0x40,
        "F18" => 0x4F,
        "F19" => 0x50,
        "F20" => 0x5A,
        _ => return None,
    })
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
    let title = normal_window_for(&app)
        .and_then(|window| window_title(&window))
        .unwrap_or_default();
    Ok(Frontmost {
        app_name,
        process,
        pid: app.processIdentifier(),
        title,
    })
}

pub fn frontmost_bundle_is(expected: &str) -> bool {
    frontmost_application()
        .and_then(|app| app.bundleIdentifier())
        .is_some_and(|bundle| bundle.to_string() == expected)
}

pub fn running_bundle_ids() -> Vec<String> {
    let applications = NSWorkspace::sharedWorkspace().runningApplications();
    applications
        .to_vec()
        .into_iter()
        .filter_map(|app| app.bundleIdentifier().map(|bundle| bundle.to_string()))
        .collect()
}

/// Routes line scroll events through the visible normal-layer window after
/// confirming that the expected bundle still owns the foreground.
pub fn scroll(notches: i32, x: f64, y: f64, expected_bundle: &str) -> Result<()> {
    post_event_access()?;
    let (_, window) = frontmost_window().ok_or_else(|| anyhow!(SCROLL_TARGET_UNAVAILABLE))?;
    if !frontmost_bundle_is(expected_bundle) {
        bail!(SCROLL_TARGET_UNAVAILABLE)
    }
    let bounds = window_bounds(&window).ok_or_else(|| anyhow!(SCROLL_TARGET_UNAVAILABLE))?;
    let location = point_in_bounds(bounds, x, y);
    let original = CGEvent::new(None).map(|event| CGEvent::location(Some(&event)));
    if !frontmost_bundle_is(expected_bundle) {
        bail!(SCROLL_TARGET_UNAVAILABLE)
    }
    let _ = CGWarpMouseCursorPosition(location);
    if let Some(event) =
        CGEvent::new_mouse_event(None, CGEventType::MouseMoved, location, CGMouseButton::Left)
    {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
    let _restore = CursorRestore { original };
    thread::sleep(Duration::from_millis(50));
    for _ in 0..notches.unsigned_abs() {
        if !frontmost_bundle_is(expected_bundle) {
            bail!(SCROLL_TARGET_UNAVAILABLE)
        }
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
    let app = frontmost_application()?;
    let window = normal_window_for(&app)?;
    Some((app, window))
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
    // The returned CFArray owns each record; callers only need it during this
    // function. This cannot return a borrowed record, so retain it first.
    for index in 0..windows.len() {
        // SAFETY: index is bounded by `len`, and Core Graphics guarantees a
        // CFDictionary at each position in this window-info array.
        let window = unsafe { windows.get_unchecked(index as isize) };
        let typed = unsafe { window.cast_unchecked::<CFString, CFType>() };
        if usable_window_for(
            pid,
            number(typed, unsafe { kCGWindowLayer }),
            number(typed, unsafe { kCGWindowOwnerPID }),
        ) {
            // SAFETY: the CFArray retains this record. Retaining it gives the
            // returned handle independent ownership after the array is dropped.
            let window =
                unsafe { objc2_core_foundation::CFRetained::retain(NonNull::from(window)) };
            return Some(window);
        }
    }
    None
}

fn frontmost_application() -> Option<Retained<NSRunningApplication>> {
    if let Some(mode) = unsafe { kCFRunLoopDefaultMode } {
        let _ = CFRunLoop::run_in_mode(Some(mode), 0.0, true);
    }
    NSWorkspace::sharedWorkspace().frontmostApplication()
}

fn usable_window_for(pid: i32, layer: Option<f64>, owner_pid: Option<f64>) -> bool {
    layer == Some(0.0) && owner_pid == Some(f64::from(pid))
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

    #[test]
    fn only_a_normal_window_owned_by_the_active_pid_is_usable() {
        assert!(usable_window_for(42, Some(0.0), Some(42.0)));
        assert!(!usable_window_for(42, Some(1.0), Some(42.0)));
        assert!(!usable_window_for(42, Some(0.0), Some(7.0)));
        assert!(!usable_window_for(42, None, Some(42.0)));
    }

    #[test]
    fn maps_only_macos_function_keycodes() {
        assert_eq!(function_key_code("F13"), Some(0x69));
        assert_eq!(function_key_code("F20"), Some(0x5A));
        assert_eq!(function_key_code("F21"), None);
    }
}
