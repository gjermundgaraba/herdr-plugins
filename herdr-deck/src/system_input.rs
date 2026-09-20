use std::ffi::c_void;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Enter,
    F19,
}

impl Key {
    fn keycode(self) -> u16 {
        // HIToolbox/Events.h: kVK_Return and kVK_F19.
        match self {
            Self::Enter => 0x24,
            Self::F19 => 0x50,
        }
    }

    fn flags(self) -> u64 {
        match self {
            Self::Enter => 0,
            // macOS marks F-keys with kCGEventFlagMaskSecondaryFn, even when
            // the physical Fn key is not held. Shortcut matchers rely on it.
            Self::F19 => 1 << 23,
        }
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    fn CGEventCreateKeyboardEvent(
        source: *const c_void,
        virtual_key: u16,
        key_down: bool,
    ) -> *const c_void;
    fn CGEventSetFlags(event: *const c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *const c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const c_void);
}

struct Event(*const c_void);

impl Event {
    fn new(key: Key, down: bool) -> Result<Self, String> {
        // A null source is permitted by Core Graphics. A successful create
        // returns an owned reference, released by Drop.
        let event = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), key.keycode(), down) };
        if event.is_null() {
            return Err(format!("could not create {key:?} key event"));
        }
        // Clear inherited modifiers, retaining the native function-key flag.
        unsafe { CGEventSetFlags(event, key.flags()) };
        Ok(Self(event))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // Event only contains non-null owned Core Foundation references.
        unsafe { CFRelease(self.0) };
    }
}

pub fn permitted() -> bool {
    // Read-only check: permission is granted in macOS Accessibility settings.
    unsafe { CGPreflightPostEventAccess() }
}

pub fn press(key: Key) -> Result<(), String> {
    if !permitted() {
        return Err(
            "enable the installed Herdr Deck executable in System Settings > Privacy & Security > Accessibility"
                .into(),
        );
    }
    // Allocate both events before posting either, so an allocation failure
    // cannot leave a key held down. kCGHIDEventTap = 0.
    let down = Event::new(key, true)?;
    let up = Event::new(key, false)?;
    unsafe {
        CGEventPost(0, down.0);
        CGEventPost(0, up.0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventGetType(event: *const c_void) -> u32;
        fn CGEventGetFlags(event: *const c_void) -> u64;
        fn CGEventGetIntegerValueField(event: *const c_void, field: u32) -> i64;
    }

    #[test]
    fn creates_return_and_f19_pairs_with_native_flags_without_posting() {
        for (key, keycode, flags) in [(Key::Enter, 0x24, 0), (Key::F19, 0x50, 1 << 23)] {
            for (down, event_type) in [(true, 10), (false, 11)] {
                let event = Event::new(key, down).unwrap();
                unsafe {
                    assert_eq!(CGEventGetType(event.0), event_type);
                    assert_eq!(CGEventGetFlags(event.0), flags);
                    // kCGKeyboardEventKeycode = 9.
                    assert_eq!(CGEventGetIntegerValueField(event.0, 9), keycode);
                }
            }
        }
    }
}
