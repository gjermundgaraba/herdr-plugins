mod control;
mod daemon;
mod state;

use std::{
    fs,
    io::Write,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use herdr_client::{Environment, PluginInvocation, open_rotating_log};

const DAEMON_FLAG: &str = "--daemon";
const SOCKET_TIMEOUT: Duration = Duration::from_millis(500);
const START_TIMEOUT: Duration = Duration::from_secs(2);

struct SessionPaths {
    runtime_dir: PathBuf,
    lock_file: PathBuf,
    control_socket: PathBuf,
}

impl SessionPaths {
    fn new(socket_path: &Path) -> Self {
        // SAFETY: geteuid has no preconditions.
        let runtime_dir = std::env::temp_dir().join(format!("hh-{}", unsafe { libc::geteuid() }));
        let key = runtime_key(socket_path);
        Self {
            lock_file: runtime_dir.join(format!("{key}.lock")),
            control_socket: runtime_dir.join(format!("{key}.sock")),
            runtime_dir,
        }
    }
}

fn main() -> ExitCode {
    let daemon = std::env::args().nth(1).as_deref() == Some(DAEMON_FLAG);
    match if daemon {
        daemon::run_daemon()
    } else {
        run_invocation()
    } {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if daemon {
                log_error(&format!("{error:#}"));
            } else {
                eprintln!("herdr-history: {error:#}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run_invocation() -> Result<()> {
    let environment = Environment::load()?;
    environment.require_plugin()?;
    let socket_path = environment
        .socket_path
        .as_deref()
        .context("HERDR_SOCKET_PATH is not set")?;
    let paths = SessionPaths::new(socket_path);
    let build = build_hash()?;

    match environment.invocation() {
        Some(PluginInvocation::Startup) | Some(PluginInvocation::Action("activate")) => {
            control::activate_daemon(&paths.control_socket, &build)
        }
        Some(PluginInvocation::Action("back")) => {
            control::run_active_command(&paths.control_socket, "back", &build)
        }
        Some(PluginInvocation::Action("forward")) => {
            control::run_active_command(&paths.control_socket, "forward", &build)
        }
        invocation => bail!("unknown Herdr invocation: {invocation:?}"),
    }
}

/// Identity is content: identical bytes never swap, changed bytes always do.
/// Hashed from the executable's path, so a rebuild landing in the instant
/// between daemon spawn and its self-hash stores the new file's hash under
/// old code; that misses one swap and self-heals on the next rebuild.
fn build_hash() -> Result<String> {
    let executable = std::env::current_exe().context("cannot resolve executable")?;
    fs::read(&executable)
        .map(fnv)
        .with_context(|| format!("cannot read {}", executable.display()))
}

fn runtime_key(path: &Path) -> String {
    fnv(path.as_os_str().as_bytes().iter().copied())
}

fn fnv(bytes: impl IntoIterator<Item = u8>) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{:024x}", hash & ((1_u128 << 96) - 1))
}

fn remove_socket(path: &Path) -> Result<()> {
    herdr_client::unix::remove_socket(path)
        .with_context(|| format!("cannot remove {}", path.display()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn log_error(message: &str) {
    let path = Environment::load()
        .ok()
        .and_then(|environment| environment.require_plugin().ok())
        .map(|plugin| plugin.logs_dir().join("history.log"));
    if let Some(Ok(mut file)) = path.map(|path| open_rotating_log(&path, 10 << 20, 3)) {
        let _ = writeln!(file, "{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_daemon_per_herdr_server() {
        let first = SessionPaths::new(Path::new("/tmp/herdr.sock"));
        let same_server = SessionPaths::new(Path::new("/tmp/herdr.sock"));
        let other_server = SessionPaths::new(Path::new("/tmp/other-herdr.sock"));

        assert_eq!(first.lock_file, same_server.lock_file);
        assert_eq!(first.control_socket, same_server.control_socket);
        assert_ne!(first.control_socket, other_server.control_socket);
        assert!(first.control_socket.as_os_str().len() < 104);
    }

    #[test]
    fn build_identity_is_content_not_metadata() {
        assert_eq!(fnv(b"same".to_vec()), fnv(b"same".to_vec()));
        assert_ne!(fnv(b"same".to_vec()), fnv(b"diff".to_vec()));
        assert_eq!(build_hash().unwrap().len(), 24);
    }

    #[test]
    fn manifest_and_crate_versions_move_together() {
        let manifest = include_str!("../herdr-plugin.toml");
        let version = manifest
            .lines()
            .find_map(|line| line.strip_prefix("version = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .expect("herdr-plugin.toml declares a version");
        assert_eq!(
            version,
            env!("CARGO_PKG_VERSION"),
            "herdr-plugin.toml and Cargo.toml versions must move together: \
             display-only since the daemon handshake compares binary hashes, \
             but drift confuses `herdr plugin list` and releases"
        );
    }
}
