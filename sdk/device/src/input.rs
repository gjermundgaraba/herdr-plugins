//! Prompts and Enter as keyboard events for the frontmost app. Herdr is not
//! involved: the app is the followed Herdr client because [`Focus`] says its
//! window has focus.

use std::time::Duration;

use herdr_client::follow::ClientKey;

use crate::{Focus, Refusal};

/// How long typed text settles before Enter follows. Terminal agents that
/// detect pasted bursts treat an Enter arriving with the text as a newline;
/// this is longer than their burst windows.
pub const SUBMIT_SETTLE: Duration = Duration::from_millis(200);

/// UTF-16 units one keyboard event carries. Core Graphics accepts more, but
/// receivers are only reliable up to this many per event.
pub const TEXT_CHUNK_UNITS: usize = 20;

/// A prompt or submit, as a device binding describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OsInput {
    /// Type the text.
    Text(String),
    /// Press Enter.
    Enter,
}

impl OsInput {
    /// A prompt: its text, then Enter unless `submit` is false.
    pub fn prompt(text: &str, submit: bool) -> Vec<Self> {
        let mut inputs = vec![Self::Text(text.to_owned())];
        if submit {
            inputs.push(Self::Enter);
        }
        inputs
    }
}

/// One posted key press, down and up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keystroke<'a> {
    /// Types these UTF-16 units, at most [`TEXT_CHUNK_UNITS`] of them.
    Text(&'a [u16]),
    /// Return, with no modifiers.
    Enter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PostError {
    #[error(
        "macOS post-event access is denied: enable this executable in \
         System Settings > Privacy & Security > Accessibility"
    )]
    AccessDenied,
    #[error("could not create a keyboard event")]
    CreateEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    #[error(transparent)]
    Refused(#[from] Refusal),
    #[error(transparent)]
    Post(#[from] PostError),
}

/// `text` cut into the strings of successive keystrokes. A surrogate pair is
/// never split between two of them.
pub fn text_chunks(text: &str) -> Vec<Vec<u16>> {
    let mut chunks = Vec::new();
    let mut chunk = Vec::with_capacity(TEXT_CHUNK_UNITS);
    let mut units = [0; 2];
    for character in text.chars() {
        let encoded = character.encode_utf16(&mut units);
        if chunk.len() + encoded.len() > TEXT_CHUNK_UNITS {
            chunks.push(std::mem::take(&mut chunk));
        }
        chunk.extend_from_slice(encoded);
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

/// Types `inputs` captured for `client`, which must be the followed client
/// with its window focused. `focus` is checked before every keystroke:
/// each chunk of text and each Enter. Once it refuses, nothing
/// more is sent, and nothing is retargeted. `settle` runs before an Enter
/// that follows text, so the receiver has taken the text first.
pub fn type_inputs(
    inputs: &[OsInput],
    client: &ClientKey,
    focus: &Focus,
    post: &dyn Fn(Keystroke<'_>) -> Result<(), PostError>,
    settle: &dyn Fn(),
) -> Result<(), InputError> {
    for (index, input) in inputs.iter().enumerate() {
        match input {
            OsInput::Text(text) => {
                for chunk in text_chunks(text) {
                    focus.check_focused(client)?;
                    post(Keystroke::Text(&chunk))?;
                }
            }
            OsInput::Enter => {
                if index > 0 {
                    settle();
                }
                focus.check_focused(client)?;
                post(Keystroke::Enter)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::focus::tests::{client, focused_on};

    /// Types `inputs` into a recorder, never the OS; `during` runs after
    /// each recorded keystroke and at each settle.
    fn typed(
        inputs: &[OsInput],
        focus: &Focus,
        during: &dyn Fn(usize),
    ) -> (Result<(), InputError>, Vec<String>) {
        let events = RefCell::new(Vec::new());
        let record = |line: String| {
            events.borrow_mut().push(line);
            during(events.borrow().len());
        };
        let result = type_inputs(
            inputs,
            &client("work"),
            focus,
            &|keystroke| {
                record(match keystroke {
                    Keystroke::Text(units) => String::from_utf16(units).unwrap(),
                    Keystroke::Enter => "enter".into(),
                });
                Ok(())
            },
            &|| record("settle".into()),
        );
        (result, events.into_inner())
    }

    #[test]
    fn text_is_chunked_without_splitting_a_character() {
        assert!(text_chunks("").is_empty());
        assert_eq!(
            text_chunks("/model"),
            ["/model".encode_utf16().collect::<Vec<_>>()]
        );
        // Nineteen units, then a two-unit character that must move on whole.
        let text = format!("{}🙂tail", "a".repeat(19));
        assert_eq!(
            text_chunks(&text)
                .iter()
                .map(|chunk| String::from_utf16(chunk).unwrap())
                .collect::<Vec<_>>(),
            ["a".repeat(19), "🙂tail".to_owned()]
        );
        let long = "é".repeat(45);
        let chunks = text_chunks(&long);
        assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), [20, 20, 5]);
        assert_eq!(String::from_utf16(&chunks.concat()).unwrap(), long);
    }

    #[test]
    fn prompts_type_their_chunks_settle_and_press_enter() {
        assert_eq!(
            OsInput::prompt("draft", false),
            [OsInput::Text("draft".into())]
        );
        let focus = focused_on(Some(client("work")));
        let text = format!("{}tail", "a".repeat(20));
        let (result, events) = typed(&OsInput::prompt(&text, true), &focus, &|_| {});
        result.unwrap();
        assert_eq!(events, ["a".repeat(20).as_str(), "tail", "settle", "enter"]);
        // A lone Enter does not settle.
        let (result, events) = typed(&[OsInput::Enter], &focus, &|_| {});
        result.unwrap();
        assert_eq!(events, ["enter"]);
    }

    #[test]
    fn focus_is_checked_before_every_chunk_and_the_enter() {
        let text = format!("{}tail", "a".repeat(20));
        let prompt = OsInput::prompt(&text, true);
        let (result, events) = typed(&prompt, &focused_on(None), &|_| {});
        assert_eq!(result, Err(Refusal::NotFollowed.into()));
        assert!(events.is_empty());
        // The followed client is behind another app: nothing is typed.
        let behind = focused_on(None);
        behind.set(Some(client("work")), false);
        let (result, events) = typed(&prompt, &behind, &|_| {});
        assert_eq!(result, Err(Refusal::NotFocused.into()));
        assert!(events.is_empty());
        // Focus is lost after each step in turn: what was sent stays sent,
        // and no keystroke follows.
        let chunk = "a".repeat(20);
        for (lost_after, expected) in [
            (1, vec![chunk.as_str()]),
            (2, vec![chunk.as_str(), "tail", "settle"]),
            (3, vec![chunk.as_str(), "tail", "settle"]),
        ] {
            // Either the window loses focus, or another client is followed.
            for (followed, refusal) in [
                (Some(client("work")), Refusal::NotFocused),
                (Some(client("default")), Refusal::NotFollowed),
                (None, Refusal::NotFollowed),
            ] {
                let focus = focused_on(Some(client("work")));
                let (result, events) = typed(&prompt, &focus, &|count| {
                    if count == lost_after {
                        focus.set(followed.clone(), false);
                    }
                });
                assert_eq!(result, Err(refusal.into()), "{lost_after}");
                assert_eq!(events, expected, "{lost_after}");
            }
        }

        // Stopping while the text settles: no Enter.
        let focus = focused_on(Some(client("work")));
        let (result, events) = typed(&prompt, &focus, &|count| {
            if count == 3 {
                focus.stop();
            }
        });
        assert_eq!(result, Err(Refusal::Stopping.into()));
        assert_eq!(events.last().map(String::as_str), Some("settle"));
    }

    #[test]
    fn a_failed_keystroke_is_not_followed_by_enter() {
        let focus = focused_on(Some(client("work")));
        let posted = RefCell::new(0);
        let result = type_inputs(
            &OsInput::prompt("hello", true),
            &client("work"),
            &focus,
            &|_| {
                *posted.borrow_mut() += 1;
                Err(PostError::AccessDenied)
            },
            &|| panic!("settled after a failure"),
        );
        assert_eq!(result, Err(PostError::AccessDenied.into()));
        assert_eq!(posted.into_inner(), 1);
    }
}
