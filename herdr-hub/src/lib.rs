mod config;
mod discover;
mod hub;
mod model;
mod relay;
mod remote;
pub(crate) mod server;
mod watch;

pub use hub::run;

pub fn run_relay(
    resolve_herdr: impl FnOnce() -> anyhow::Result<std::path::PathBuf>,
) -> anyhow::Result<()> {
    relay::run(resolve_herdr)
}

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
