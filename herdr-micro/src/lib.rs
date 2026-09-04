pub mod actions;
pub mod config;
pub mod control;
pub mod daemon;
pub mod doctor;
pub mod gestures;
pub mod hub;
pub mod macos;
pub mod process;
pub mod protocol;
pub mod setup;

pub const PLUGIN_ID: &str = "gjermundgaraba.herdr-micro";

pub fn plugin_paths() -> anyhow::Result<herdr_client::PluginPaths> {
    Ok(herdr_client::Environment::load()?.require_plugin()?)
}
