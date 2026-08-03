pub mod actions;
pub mod config;
pub mod control;
pub mod daemon;
pub mod doctor;
pub mod gestures;
pub mod ghostty;
pub mod herdr;
pub mod macos;
pub mod protocol;
pub mod setup;

pub use codex_micro as device;

pub const PLUGIN_ID: &str = "gjermundgaraba.herdr-micro";
