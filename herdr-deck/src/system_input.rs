//! The `system-key` keys: posted to the frontmost macOS app regardless of
//! Herdr. Prompts and submits go through `herdr_device` instead, which sends
//! them only while the followed Herdr client has focus.

use herdr_device::{
    PostError,
    macos::{self, CGEventFlags, Flags},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Enter,
    F19,
}

impl Key {
    fn keycode(self) -> u16 {
        // HIToolbox/Events.h: kVK_Return and kVK_F19.
        match self {
            Self::Enter => macos::RETURN_KEYCODE,
            Self::F19 => 0x50,
        }
    }

    fn flags(self) -> CGEventFlags {
        match self {
            Self::Enter => CGEventFlags(0),
            // macOS marks F-keys with the secondary-fn flag, even when the
            // physical Fn key is not held. Shortcut matchers rely on it.
            Self::F19 => CGEventFlags::MaskSecondaryFn,
        }
    }
}

/// Presses `key` with only its own flags, whatever modifiers are held.
pub fn press(key: Key) -> Result<(), PostError> {
    macos::press(key.keycode(), Flags::Exactly(key.flags()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_return_and_f19_with_their_native_flags() {
        assert_eq!(
            [Key::Enter, Key::F19].map(|key| (key.keycode(), key.flags())),
            [(0x24, CGEventFlags(0)), (0x50, CGEventFlags(1 << 23))]
        );
    }
}
