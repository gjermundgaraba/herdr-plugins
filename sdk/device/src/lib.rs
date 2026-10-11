//! What a Herdr device tool does when a control is pressed, shared by Micro
//! and Deck.
//!
//! A press is captured against the followed client
//! ([`herdr_client::follow::Directory::followed_client`]) and becomes either
//! a `client.jump` ([`JumpDispatcher`]) or keyboard events for the frontmost
//! app ([`type_inputs`]). Both run some time after the press, so both are
//! checked against [`Focus`], which holds what the follower last published
//! and whether the tool is stopping. A jump needs its client to be the
//! followed one still. Keyboard events also need that client's window to
//! have focus, because they go to whatever app is in front.
//!
//! The check reads a published report, not the OS. A client's report of
//! losing focus reaches the tool some time after the other app took it, and
//! the chunks of a prompt (at most [`TEXT_CHUNK_UNITS`] UTF-16 units each)
//! are posted back to back, so chunks of a prompt can land in that app
//! until the report arrives. From then on nothing more of it is posted:
//! not a later chunk, and not its Enter, which waits [`SUBMIT_SETTLE`]
//! after the text.

mod focus;
mod input;
mod jumps;
#[cfg(target_os = "macos")]
pub mod macos;

pub use focus::{Focus, Refusal, selection_change};
pub use input::{
    InputError, Keystroke, OsInput, PostError, SUBMIT_SETTLE, TEXT_CHUNK_UNITS, text_chunks,
    type_inputs,
};
pub use jumps::{JumpDispatcher, JumpError};
