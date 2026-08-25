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
use herdr_client::{Environment, PluginInvocation, hash::fnv, open_rotating_log};

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

fn remove_socket(path: &Path) -> Result<()> {
    herdr_client::unix::remove_socket(path)
        .with_context(|| format!("cannot remove {}", path.display()))
}

fn now_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
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
        assert_eq!(build_hash().unwrap(), build_hash().unwrap());
        assert_eq!(build_hash().unwrap().len(), 24);
    }
}
