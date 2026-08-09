use anyhow::{Context, Result, bail};
use std::{
    env,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::Duration,
};

use crate::hid::{HELPER_LABEL, HELPER_VERSION, HID_LAUNCH_SOCKET_NAME, hid_socket_path};

pub const HELPER_BINARY_NAME: &str = "herdr-micro-hid";
pub const HELPER_PATH: &str = "/Library/PrivilegedHelperTools/dev.herdr.herdr-micro-hid";
pub const PLIST_PATH: &str = "/Library/LaunchDaemons/dev.herdr.herdr-micro-hid.plist";
// One in-flight request, Layer 1 recovery, native close, and one second of margin.
const HELPER_EXIT_TIMEOUT_SECONDS: u64 = 16;
const BOOTSTRAP_RETRY_DELAYS: [Duration; 5] = [
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

pub fn verify_installed() -> Result<()> {
    let metadata = safe_root_file(Path::new(HELPER_PATH)).context(
        "privileged USB helper is not installed; run `sudo ./bin/herdr-micro install-helper`",
    )?;
    if metadata.mode() & 0o111 == 0 {
        bail!("privileged USB helper has unsafe ownership or permissions; reinstall it")
    }
    safe_root_file(Path::new(PLIST_PATH)).context(
        "privileged USB helper service is not installed; rerun `sudo ./bin/herdr-micro install-helper`",
    )?;
    let output = Command::new(HELPER_PATH)
        .arg("--version")
        .output()
        .context("inspect privileged USB helper version")?;
    if !output.status.success() {
        return command_error("inspect privileged USB helper version", output);
    }
    let installed = String::from_utf8_lossy(&output.stdout);
    let required = HELPER_VERSION;
    if installed.trim() != required {
        bail!(
            "privileged USB helper {} is installed but {} is required; rerun `sudo ./bin/herdr-micro install-helper`",
            installed.trim(),
            required
        )
    }
    command_ok(
        Command::new("/bin/launchctl")
            .arg("print")
            .arg(format!("system/{HELPER_LABEL}")),
        "inspect privileged USB helper service",
    )?;
    Ok(())
}

fn safe_root_file(path: &Path) -> Result<fs::Metadata> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        bail!("{} has unsafe ownership or permissions", path.display())
    }
    Ok(metadata)
}

pub fn install() -> Result<()> {
    require_root()?;
    let uid = sudo_uid()?;
    ensure_install_dir(
        Path::new(HELPER_PATH)
            .parent()
            .context("helper install path has no parent")?,
        0,
    )?;
    let source = env::current_exe()
        .context("locate herdr-micro executable")?
        .parent()
        .context("herdr-micro executable has no parent directory")?
        .join(HELPER_BINARY_NAME);

    bootout()?;
    remove_installed_file(&hid_socket_path(uid))?;

    install_file(
        Path::new(HELPER_PATH),
        0o755,
        |target| {
            let mut source_file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&source)
                .with_context(|| format!("open helper {}", source.display()))?;
            if !source_file
                .metadata()
                .with_context(|| format!("inspect helper {}", source.display()))?
                .is_file()
            {
                bail!("helper is not a regular file: {}", source.display());
            }
            io::copy(&mut source_file, target).context("copy helper")?;
            Ok(())
        },
        |_| Ok(()),
    )?;

    let plist = render_plist(uid);
    install_file(
        Path::new(PLIST_PATH),
        0o600,
        |target| {
            target
                .write_all(plist.as_bytes())
                .context("write launchd plist")
        },
        validate_plist,
    )?;

    bootstrap(uid)?;
    verify_installed()
}

fn ensure_install_dir(path: &Path, owner: libc::uid_t) -> Result<()> {
    match fs::DirBuilder::new().mode(0o755).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
    }
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        bail!("{} has unsafe ownership or permissions", path.display())
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    require_root()?;
    bootout()?;

    let mut paths = vec![PathBuf::from(PLIST_PATH), PathBuf::from(HELPER_PATH)];
    if let Some(uid) = env::var_os("SUDO_UID")
        .as_deref()
        .and_then(|value| parse_sudo_uid(Some(value)).ok())
    {
        paths.push(hid_socket_path(uid));
    }
    for path in paths {
        remove_installed_file(&path)?;
    }
    Ok(())
}

fn remove_installed_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_dir(path.parent().context("installed path has no parent")?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

fn require_root() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("helper installation must run as root (use sudo)");
    }
    Ok(())
}

fn sudo_uid() -> Result<libc::uid_t> {
    parse_sudo_uid(env::var_os("SUDO_UID").as_deref())
}

fn parse_sudo_uid(value: Option<&OsStr>) -> Result<libc::uid_t> {
    let value = value.context("SUDO_UID is missing; run through sudo as a non-root user")?;
    let text = value.to_str().context("SUDO_UID is not valid UTF-8")?;
    let uid = text
        .parse::<libc::uid_t>()
        .context("SUDO_UID must be a numeric user ID")?;
    if uid == 0 {
        bail!("SUDO_UID must identify a non-root user");
    }
    Ok(uid)
}

fn render_plist(uid: libc::uid_t) -> String {
    let socket = hid_socket_path(uid);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{HELPER_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{HELPER_PATH}</string>
    <string>{uid}</string>
  </array>
  <key>Sockets</key>
  <dict>
    <key>{HID_LAUNCH_SOCKET_NAME}</key>
    <dict>
      <key>SockFamily</key>
      <string>Unix</string>
      <key>SockType</key>
      <string>stream</string>
      <key>SockPathName</key>
      <string>{}</string>
      <key>SockPathOwner</key>
      <integer>{uid}</integer>
      <key>SockPathGroup</key>
      <integer>0</integer>
      <key>SockPathMode</key>
      <integer>384</integer>
    </dict>
  </dict>
  <key>ThrottleInterval</key>
  <integer>1</integer>
  <key>ExitTimeOut</key>
  <integer>{HELPER_EXIT_TIMEOUT_SECONDS}</integer>
  <key>Umask</key>
  <integer>63</integer>
</dict>
</plist>
"#,
        socket.display()
    )
}

fn install_file(
    target: &Path,
    mode: u32,
    write: impl FnOnce(&mut File) -> Result<()>,
    validate: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let parent = target.parent().context("install target has no parent")?;
    let name = target
        .file_name()
        .context("install target has no file name")?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        String::from_utf8_lossy(name.as_bytes()),
        std::process::id()
    ));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        let fd = file.as_raw_fd();
        if unsafe { libc::fchown(fd, 0, 0) } == -1 {
            return Err(io::Error::last_os_error()).context("set installed file ownership");
        }
        if unsafe { libc::fchmod(fd, mode as libc::mode_t) } == -1 {
            return Err(io::Error::last_os_error()).context("set installed file mode");
        }
        write(&mut file)?;
        file.sync_all().context("sync installed file")?;
        drop(file);
        validate(&temporary)?;
        fs::rename(&temporary, target).with_context(|| format!("install {}", target.display()))?;
        sync_dir(parent)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn validate_plist(path: &Path) -> Result<()> {
    command_ok(
        Command::new("/usr/bin/plutil").arg("-lint").arg(path),
        "validate launchd plist",
    )
}

fn bootout() -> Result<()> {
    let service = format!("system/{HELPER_LABEL}");
    let output = Command::new("/bin/launchctl")
        .arg("bootout")
        .arg(&service)
        .output()
        .context("run launchctl bootout")?;
    if output.status.success() || is_not_loaded(&output) {
        return Ok(());
    }
    command_error("bootout helper", output)
}

fn bootstrap(uid: libc::uid_t) -> Result<()> {
    let socket = hid_socket_path(uid);
    let mut output = run_bootstrap()?;
    for delay in BOOTSTRAP_RETRY_DELAYS {
        if output.status.success() {
            return Ok(());
        }
        if !is_transient_bootstrap_error(&output) {
            return command_error("bootstrap helper", output);
        }
        thread::sleep(delay);
        if service_is_loaded() {
            return Ok(());
        }
        remove_installed_file(&socket)?;
        output = run_bootstrap()?;
    }
    if output.status.success() || (is_transient_bootstrap_error(&output) && service_is_loaded()) {
        Ok(())
    } else {
        command_error("bootstrap helper after transient launchd retries", output)
    }
}

fn is_transient_bootstrap_error(output: &Output) -> bool {
    output.status.code() == Some(libc::EIO)
}

fn run_bootstrap() -> Result<Output> {
    Command::new("/bin/launchctl")
        .arg("bootstrap")
        .arg("system")
        .arg(PLIST_PATH)
        .output()
        .context("run command to bootstrap helper")
}

fn service_is_loaded() -> bool {
    Command::new("/bin/launchctl")
        .arg("print")
        .arg(format!("system/{HELPER_LABEL}"))
        .output()
        .is_ok_and(|output| output.status.success())
}

fn is_not_loaded(output: &Output) -> bool {
    output.status.code() == Some(3)
        && (output
            .stderr
            .windows(b"No such process".len())
            .any(|part| part == b"No such process")
            || output
                .stderr
                .windows(b"Could not find service".len())
                .any(|part| part == b"Could not find service"))
}

fn command_ok(command: &mut Command, action: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("run command to {action}"))?;
    if output.status.success() {
        Ok(())
    } else {
        command_error(action, output)
    }
}

fn command_error(action: &str, output: Output) -> Result<()> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    bail!(
        "{action} failed ({}): {}{}",
        output.status,
        stderr.trim(),
        if stdout.is_empty() {
            String::new()
        } else {
            format!("\n{}", stdout.trim())
        }
    )
}

fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{fs::PermissionsExt, process::ExitStatusExt};

    #[test]
    fn parses_only_non_root_sudo_uid() {
        assert_eq!(parse_sudo_uid(Some(OsStr::new("501"))).unwrap(), 501);
        assert!(parse_sudo_uid(None).is_err());
        assert!(parse_sudo_uid(Some(OsStr::new("0"))).is_err());
        assert!(parse_sudo_uid(Some(OsStr::new("alice"))).is_err());
        assert!(parse_sudo_uid(Some(OsStr::new("-1"))).is_err());
    }

    #[test]
    fn creates_and_validates_install_directory() {
        let base =
            std::env::temp_dir().join(format!("herdr-micro-helper-install-{}", std::process::id()));
        let leaf = base.join("PrivilegedHelperTools");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir(&base).unwrap();
        // SAFETY: getuid has no preconditions.
        ensure_install_dir(&leaf, unsafe { libc::getuid() }).unwrap();
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ensure_install_dir(&leaf, unsafe { libc::getuid() }).is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn retries_only_launchd_eio() {
        let output = |code| Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert!(is_transient_bootstrap_error(&output(libc::EIO)));
        assert!(!is_transient_bootstrap_error(&output(libc::EPERM)));
    }

    #[test]
    fn renders_on_demand_root_owned_socket_plist() {
        let plist = render_plist(501);
        assert!(plist.contains(&format!("<string>{HELPER_LABEL}</string>")));
        assert!(plist.contains(&format!("<string>{HELPER_PATH}</string>")));
        assert!(plist.contains("<string>501</string>"));
        assert!(plist.contains("dev.herdr.herdr-micro-hid-501.sock"));
        assert!(plist.contains("<key>SockPathOwner</key>\n      <integer>501</integer>"));
        assert!(plist.contains("<key>SockPathMode</key>\n      <integer>384</integer>"));
        assert!(plist.contains("<key>ThrottleInterval</key>\n  <integer>1</integer>"));
        assert!(plist.contains(&format!(
            "<key>ExitTimeOut</key>\n  <integer>{HELPER_EXIT_TIMEOUT_SECONDS}</integer>"
        )));
        assert!(plist.contains("<key>Umask</key>\n  <integer>63</integer>"));
        assert!(!plist.contains("KeepAlive"));
        assert!(!plist.contains("RunAtLoad"));
    }
}
