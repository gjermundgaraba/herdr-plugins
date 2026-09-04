mod config;
mod discover;
mod hub;
mod model;
mod relay;
mod remote;
pub(crate) mod server;
mod watch;

pub use hub::run;

pub use relay::run as run_relay;

pub fn check_remote_hosts() -> anyhow::Result<Vec<(String, String)>> {
    config::load()?
        .hosts
        .into_iter()
        .map(|host| {
            let version = remote::check_version(&host)?;
            Ok((host.key, version))
        })
        .collect()
}
