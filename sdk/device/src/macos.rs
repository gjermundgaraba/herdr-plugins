//! Keystrokes posted to the frontmost macOS app through Core Graphics. Each
//! needs the executable to have Accessibility access.

use objc2_core_foundation::CFRetained;
pub use objc2_core_graphics::CGEventFlags;
use objc2_core_graphics::{CGEvent, CGEventTapLocation, CGPreflightPostEventAccess};

use crate::{Keystroke, PostError};

/// HIToolbox/Events.h `kVK_Return`.
pub const RETURN_KEYCODE: u16 = 0x24;

/// Whether this process may post keyboard events. A read-only check: access
/// is granted in System Settings.
pub fn permitted() -> bool {
    CGPreflightPostEventAccess()
}

/// Posts one keystroke to the frontmost app, with no modifiers whatever the
/// user is physically holding. Text is read from the event's string, not its
/// keycode, so the keyboard layout does not matter.
pub fn post(keystroke: Keystroke<'_>) -> Result<(), PostError> {
    require_permission()?;
    post_pair(keystroke_events(keystroke)?);
    Ok(())
}

/// How the flags of a pressed key relate to the ones Core Graphics gives a
/// new keyboard event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flags {
    /// Exactly these, replacing any modifier the user is physically holding.
    Exactly(CGEventFlags),
    /// The event's own flags and these. Core Graphics derives masks that
    /// hotkey listeners match on, such as secondary-fn on F-keys.
    Added(CGEventFlags),
}

/// Presses `keycode` with `flags`.
pub fn press(keycode: u16, flags: Flags) -> Result<(), PostError> {
    require_permission()?;
    post_pair([
        key_event(keycode, flags, true)?,
        key_event(keycode, flags, false)?,
    ]);
    Ok(())
}

fn require_permission() -> Result<(), PostError> {
    if permitted() {
        Ok(())
    } else {
        Err(PostError::AccessDenied)
    }
}

/// Both events exist before either is posted, so a failure cannot leave a
/// key held down.
fn post_pair(events: [CFRetained<CGEvent>; 2]) {
    for event in &events {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event));
    }
}

fn key_event(keycode: u16, flags: Flags, down: bool) -> Result<CFRetained<CGEvent>, PostError> {
    let event = CGEvent::new_keyboard_event(None, keycode, down).ok_or(PostError::CreateEvent)?;
    match flags {
        Flags::Exactly(flags) => CGEvent::set_flags(Some(&event), flags),
        Flags::Added(added) if added.is_empty() => {}
        Flags::Added(added) => {
            CGEvent::set_flags(Some(&event), CGEvent::flags(Some(&event)) | added)
        }
    }
    Ok(event)
}

/// The key down and up events of `keystroke`, not yet posted.
fn keystroke_events(keystroke: Keystroke<'_>) -> Result<[CFRetained<CGEvent>; 2], PostError> {
    let plain = |keycode, down| key_event(keycode, Flags::Exactly(CGEventFlags(0)), down);
    match keystroke {
        Keystroke::Enter => Ok([plain(RETURN_KEYCODE, true)?, plain(RETURN_KEYCODE, false)?]),
        Keystroke::Text(units) => {
            let text = |down| {
                let event = plain(0, down)?;
                // SAFETY: `units` is a live buffer of exactly `units.len()`
                // UTF-16 units, and Core Graphics copies it before returning.
                unsafe {
                    CGEvent::keyboard_set_unicode_string(
                        Some(&event),
                        units.len() as _,
                        units.as_ptr(),
                    );
                }
                Ok(event)
            };
            Ok([text(true)?, text(false)?])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_chunks;
    use objc2_core_graphics::{CGEventField, CGEventType};

    fn typed(event: &CGEvent) -> String {
        let mut buffer = [0_u16; 64];
        let mut length = 0;
        // SAFETY: the buffer holds `buffer.len()` units and `length` is a
        // valid out pointer.
        unsafe {
            CGEvent::keyboard_get_unicode_string(
                Some(event),
                buffer.len() as _,
                &mut length,
                buffer.as_mut_ptr(),
            );
        }
        String::from_utf16(&buffer[..length as usize]).unwrap()
    }

    fn keycode(event: &CGEvent) -> i64 {
        CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode)
    }

    fn is_down_then_up(events: &[CFRetained<CGEvent>; 2]) -> bool {
        CGEvent::r#type(Some(&events[0])) == CGEventType::KeyDown
            && CGEvent::r#type(Some(&events[1])) == CGEventType::KeyUp
    }

    #[test]
    fn text_keystrokes_carry_their_string_and_no_modifiers_without_posting() {
        let text = format!("{}🙂 /model", "ü".repeat(19));
        let mut down_text = String::new();
        for chunk in text_chunks(&text) {
            let events = keystroke_events(Keystroke::Text(&chunk)).unwrap();
            assert!(is_down_then_up(&events));
            for event in &events {
                assert_eq!(CGEvent::flags(Some(event)), CGEventFlags(0));
            }
            assert_eq!(typed(&events[1]), typed(&events[0]));
            down_text.push_str(&typed(&events[0]));
        }
        assert_eq!(down_text, text);
    }

    #[test]
    fn enter_is_a_plain_return_and_a_press_carries_exactly_its_flags() {
        let events = keystroke_events(Keystroke::Enter).unwrap();
        assert!(is_down_then_up(&events));
        for event in &events {
            assert_eq!(CGEvent::flags(Some(event)), CGEventFlags(0));
            assert_eq!(keycode(event), i64::from(RETURN_KEYCODE));
        }
        // kVK_F19 with the function-key flag macOS gives F-keys.
        for down in [true, false] {
            let flags = Flags::Exactly(CGEventFlags::MaskSecondaryFn);
            let event = key_event(0x50, flags, down).unwrap();
            assert_eq!(CGEvent::flags(Some(&event)), CGEventFlags::MaskSecondaryFn);
            assert_eq!(keycode(&event), 0x50);
        }
    }

    #[test]
    fn added_flags_keep_the_ones_a_new_event_has() {
        for down in [true, false] {
            let own = CGEvent::flags(Some(
                &CGEvent::new_keyboard_event(None, 0x50, down).unwrap(),
            ));
            for added in [
                CGEventFlags(0),
                CGEventFlags::MaskCommand | CGEventFlags::MaskShift,
            ] {
                let event = key_event(0x50, Flags::Added(added), down).unwrap();
                assert_eq!(CGEvent::flags(Some(&event)), own | added);
                assert_eq!(keycode(&event), 0x50);
            }
        }
    }
}
