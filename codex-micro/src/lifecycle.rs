//! Per-user launchd installation for the Codex Micro service.

use std::{
    env,
    ffi::{CStr, OsStr},
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use tempfile::NamedTempFile;

use crate::{
    InputMonitoringAccess,
    service::{Client, ServiceStatus},
};

const LABEL: &str = "dev.herdr.codex-micro";
const WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const WAIT_INTERVAL: Duration = Duration::from_millis(50);
const BOOTOUT_TIMEOUT: Duration = Duration::from_secs(15);
const LIFECYCLE_LOCK_TIMEOUT: Duration = Duration::from_secs(15);
const EXECUTABLE_MODE: u32 = 0o700;
const PLIST_MODE: u32 = 0o600;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Paths {
    home: PathBuf,
    application_dir: PathBuf,
    executable: PathBuf,
    plist: PathBuf,
    log_dir: PathBuf,
    log: PathBuf,
}

pub fn install() -> Result<ServiceStatus> {
    require_user()?;
    let _lock = LifecycleLock::acquire()?;
    let paths = paths()?;
    ensure_managed_dirs(&paths)?;

    let source = env::current_exe().context("locate codex-micro executable")?;
    let executable = read_regular_file(&source)
        .with_context(|| format!("read executable {}", source.display()))?;
    let plist = render_plist(&paths).into_bytes();
    let uid = effective_uid();
    let executable_changed =
        !managed_file_matches(&paths.executable, &executable, uid, EXECUTABLE_MODE)?;
    let plist_changed = !managed_file_matches(&paths.plist, &plist, uid, PLIST_MODE)?;

    let staged_executable = executable_changed
        .then(|| stage_file(&paths.executable, &executable, EXECUTABLE_MODE))
        .transpose()?;
    let staged_plist = plist_changed
        .then(|| stage_file(&paths.plist, &plist, PLIST_MODE))
        .transpose()?;

    if executable_changed || plist_changed {
        bootout()?;
        if let Some(staged) = staged_executable {
            commit_file(staged, &paths.executable)?;
        }
        if let Some(staged) = staged_plist {
            commit_file(staged, &paths.plist)?;
        }
    }

    command_ok(
        Command::new("/bin/launchctl")
            .arg("enable")
            .arg(service_target()),
        "enable Codex Micro service",
    )?;
    if !is_loaded()? {
        bootstrap(&paths.plist)?;
    }
    wait_for_status()
}

pub fn status() -> Result<ServiceStatus> {
    require_user()?;
    let (events, _) = mpsc::channel();
    Client::connect(events)?.status()
}

/// Requests Input Monitoring from the installed service process.
pub fn authorize() -> Result<()> {
    require_user()?;
    install().context("install Codex Micro service before authorization")?;
    let access = {
        let (events, _) = mpsc::channel();
        Client::connect(events)?.authorize()?
    };
    let access = if access == InputMonitoringAccess::Granted {
        access
    } else {
        let _lock = LifecycleLock::acquire()?;
        let paths = paths()?;
        bootout()?;
        bootstrap(&paths.plist)?;
        wait_for_status()?.input_monitoring
    };
    println!(
        "{}",
        match access {
            InputMonitoringAccess::Granted => "granted",
            InputMonitoringAccess::Denied => "denied",
            InputMonitoringAccess::Unknown => "unknown",
        }
    );
    Ok(())
}

pub fn uninstall() -> Result<()> {
    require_user()?;
    let _lock = LifecycleLock::acquire()?;
    let paths = paths()?;
    let plist_parent_exists = safe_parent_exists(&paths, &paths.plist)?;
    let executable_parent_exists = safe_parent_exists(&paths, &paths.executable)?;
    bootout()?;
    if plist_parent_exists {
        remove_file(&paths.plist)?;
    }
    if executable_parent_exists {
        remove_file(&paths.executable)?;
    }
    match fs::remove_dir(&paths.application_dir) {
        Ok(()) => sync_dir(
            paths
                .application_dir
                .parent()
                .context("application directory has no parent")?,
        ),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(error) => {
            Err(error).with_context(|| format!("remove {}", paths.application_dir.display()))
        }
    }
}

fn paths() -> Result<Paths> {
    paths_from_home(&home_dir(effective_uid())?)
}

fn effective_uid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}

fn home_dir(uid: libc::uid_t) -> Result<PathBuf> {
    let mut size = 16 * 1024;
    loop {
        let mut entry = unsafe { std::mem::zeroed::<libc::passwd>() };
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0_u8; size];
        let error = unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if error == libc::ERANGE && size < 1024 * 1024 {
            size *= 2;
            continue;
        }
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error)).context("look up user home directory");
        }
        if result.is_null() || entry.pw_dir.is_null() {
            bail!("no passwd entry for user ID {uid}")
        }
        let path = PathBuf::from(OsStr::from_bytes(unsafe {
            CStr::from_ptr(entry.pw_dir).to_bytes()
        }));
        if !path.is_absolute() {
            bail!("passwd home directory must be an absolute path")
        }
        return Ok(path);
    }
}

fn paths_from_home(home: &Path) -> Result<Paths> {
    if !home.is_absolute() {
        bail!("home directory must be an absolute path")
    }
    let application_dir = home.join("Library/Application Support").join(LABEL);
    let log_dir = home.join("Library/Logs").join(LABEL);
    Ok(Paths {
        home: home.to_owned(),
        executable: application_dir.join("codex-micro"),
        plist: home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")),
        log: log_dir.join("service.log"),
        application_dir,
        log_dir,
    })
}

fn render_plist(paths: &Paths) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>serve</string>
  </array>
  <key>KeepAlive</key>
  <true/>
  <key>StandardErrorPath</key>
  <string>{}</string>
  <key>Umask</key>
  <integer>63</integer>
</dict>
</plist>
"#,
        xml_escape(&paths.executable),
        xml_escape(&paths.log),
    )
}

fn xml_escape(path: &Path) -> String {
    path.to_string_lossy()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn require_user() -> Result<()> {
    if effective_uid() == 0 {
        bail!("Codex Micro lifecycle commands refuse to run as root")
    }
    Ok(())
}

/// Dropping the owned file closes its descriptor, which releases the lock.
struct LifecycleLock(#[allow(dead_code)] File);

impl LifecycleLock {
    fn acquire() -> Result<Self> {
        let path = Path::new("/tmp").join(format!(
            "dev.herdr.codex-micro-{}.lifecycle.lock",
            effective_uid()
        ));
        Self::acquire_at(&path, LIFECYCLE_LOCK_TIMEOUT)
    }

    fn acquire_at(path: &Path, timeout: Duration) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .with_context(|| format!("open lifecycle lock {}", path.display()))?;
        let metadata = file.metadata()?;
        if !managed_metadata_matches(&metadata, effective_uid(), 0o600) {
            bail!("unsafe Codex Micro lifecycle lock {}", path.display())
        }
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self(file)),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) => {
                    return Err(error).context("lock Codex Micro lifecycle");
                }
            }
            if Instant::now() >= deadline {
                bail!("another Codex Micro lifecycle command is running")
            }
            thread::sleep(WAIT_INTERVAL);
        }
    }
}

fn ensure_managed_dirs(paths: &Paths) -> Result<()> {
    validate_home_chain(&paths.home)?;
    let library = paths.home.join("Library");
    ensure_dir(&library, false)?;
    ensure_dir(&library.join("Application Support"), false)?;
    ensure_dir(&paths.application_dir, true)?;
    ensure_dir(&library.join("LaunchAgents"), false)?;
    ensure_dir(&library.join("Logs"), false)?;
    ensure_dir(&paths.log_dir, true)
}

fn validate_home_chain(home: &Path) -> Result<()> {
    for path in home.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("inspect directory {}", path.display()))?;
        if !metadata.is_dir() || metadata.mode() & 0o022 != 0 {
            bail!(
                "{} has unsafe type or permissions in the home directory path",
                path.display()
            )
        }
    }
    let metadata = fs::symlink_metadata(home)
        .with_context(|| format!("inspect home directory {}", home.display()))?;
    if metadata.uid() != effective_uid() {
        bail!("{} is not owned by the current user", home.display())
    }
    Ok(())
}

fn safe_parent_exists(paths: &Paths, target: &Path) -> Result<bool> {
    validate_home_chain(&paths.home)?;
    let parent = target.parent().context("managed path has no parent")?;
    let relative = parent
        .strip_prefix(&paths.home)
        .with_context(|| format!("{} is outside the user home", target.display()))?;
    let mut current = paths.home.clone();
    for component in relative.components() {
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect directory {}", current.display()));
            }
        };
        if !metadata.is_dir() || metadata.uid() != effective_uid() || metadata.mode() & 0o022 != 0 {
            bail!(
                "{} has unsafe type, ownership, or permissions",
                current.display()
            )
        }
    }
    Ok(true)
}

fn ensure_dir(path: &Path, private: bool) -> Result<()> {
    match fs::DirBuilder::new()
        .mode(if private { 0o700 } else { 0o755 })
        .create(path)
    {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect directory {}", path.display()))?;
    if !metadata.is_dir()
        || metadata.uid() != effective_uid()
        || metadata.mode() & 0o022 != 0
        || (private && metadata.mode() & 0o777 != 0o700)
    {
        bail!("{} has unsafe ownership or permissions", path.display())
    }
    Ok(())
}

fn managed_file_matches(path: &Path, desired: &[u8], uid: libc::uid_t, mode: u32) -> Result<bool> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect {}", path.display()))?;
    if !managed_metadata_matches(&metadata, uid, mode) {
        return Ok(false);
    }
    let mut installed = Vec::new();
    file.read_to_end(&mut installed)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(installed == desired)
}

fn managed_metadata_matches(metadata: &fs::Metadata, uid: libc::uid_t, mode: u32) -> bool {
    metadata.is_file()
        && metadata.uid() == uid
        && metadata.nlink() == 1
        && metadata.mode() & 0o7777 == mode
}

fn read_regular_file(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn stage_file(target: &Path, bytes: &[u8], mode: u32) -> Result<NamedTempFile> {
    let parent = target.parent().context("install target has no parent")?;
    let mut file =
        NamedTempFile::new_in(parent).with_context(|| format!("stage {}", target.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write staged {}", target.display()))?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    file.as_file()
        .sync_all()
        .with_context(|| format!("sync staged {}", target.display()))?;
    Ok(file)
}

fn commit_file(file: NamedTempFile, target: &Path) -> Result<()> {
    file.persist(target)
        .map_err(|error| error.error)
        .with_context(|| format!("install {}", target.display()))?;
    sync_dir(target.parent().context("install target has no parent")?)
}

fn wait_for_status() -> Result<ServiceStatus> {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        match status() {
            Ok(status) => return Ok(status),
            Err(error) if Instant::now() >= deadline => {
                return Err(error)
                    .context("Codex Micro service did not become ready within 15 seconds");
            }
            Err(_) => {}
        }
        thread::sleep(WAIT_INTERVAL);
    }
}

fn service_domain() -> String {
    format!("gui/{}", effective_uid())
}

fn service_target() -> String {
    format!("{}/{LABEL}", service_domain())
}

fn is_loaded() -> Result<bool> {
    Ok(Command::new("/bin/launchctl")
        .arg("print")
        .arg(service_target())
        .output()
        .context("inspect Codex Micro service")?
        .status
        .success())
}

fn bootstrap(plist: &Path) -> Result<()> {
    let output = Command::new("/bin/launchctl")
        .arg("bootstrap")
        .arg(service_domain())
        .arg(plist)
        .output()
        .context("run launchctl bootstrap")?;
    if output.status.success() || is_loaded()? {
        Ok(())
    } else {
        command_error("bootstrap Codex Micro service", output)
    }
}

fn bootout() -> Result<()> {
    let mut child = Command::new("/bin/launchctl")
        .arg("bootout")
        .arg("--wait")
        .arg(service_target())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run launchctl bootout")?;
    let deadline = Instant::now() + BOOTOUT_TIMEOUT;
    loop {
        if child
            .try_wait()
            .context("wait for launchctl bootout")?
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("launchctl bootout did not finish within 15 seconds")
        }
        thread::sleep(WAIT_INTERVAL);
    }
    let output = child
        .wait_with_output()
        .context("collect launchctl bootout output")?;
    if output.status.success() || not_loaded(&output) {
        Ok(())
    } else {
        command_error("boot out Codex Micro service", output)
    }
}

fn not_loaded(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    output.status.code() == Some(3)
        && (stderr.contains("No such process") || stderr.contains("Could not find service"))
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

fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_dir(path.parent().context("removed path has no parent")?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "codex-micro-lifecycle-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        path
    }

    fn test_home_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = home_dir(effective_uid()).unwrap().join(format!(
            ".codex-micro-lifecycle-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        path
    }

    #[test]
    fn derives_exact_paths() {
        let paths = paths_from_home(Path::new("/Users/test")).unwrap();
        assert_eq!(
            paths.executable,
            Path::new("/Users/test/Library/Application Support/dev.herdr.codex-micro/codex-micro")
        );
        assert_eq!(
            paths.plist,
            Path::new("/Users/test/Library/LaunchAgents/dev.herdr.codex-micro.plist")
        );
        assert_eq!(
            paths.log,
            Path::new("/Users/test/Library/Logs/dev.herdr.codex-micro/service.log")
        );
    }

    #[test]
    fn plist_contains_only_requested_launchd_settings() {
        let plist = render_plist(&paths_from_home(Path::new("/Users/a&b")).unwrap());
        assert!(plist.contains("<string>dev.herdr.codex-micro</string>"));
        assert!(plist.contains("/Users/a&amp;b/Library/Application Support"));
        assert!(plist.contains("<string>serve</string>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(plist.contains("<key>Umask</key>\n  <integer>63</integer>"));
        assert!(!plist.contains("UserName"));
        assert!(!plist.contains("Sockets"));
    }

    #[test]
    fn managed_file_equality_requires_mode_and_single_link() {
        let dir = test_dir("file");
        let path = dir.join("managed");
        let link = dir.join("link");
        fs::write(&path, b"same").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(managed_file_matches(&path, b"same", effective_uid(), 0o600).unwrap());
        assert!(!managed_file_matches(&path, b"other", effective_uid(), 0o600).unwrap());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!managed_file_matches(&path, b"same", effective_uid(), 0o600).unwrap());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&path, &link).unwrap();
        assert!(!managed_file_matches(&path, b"same", effective_uid(), 0o600).unwrap());

        fs::remove_file(link).unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn managed_parent_rejects_group_or_world_writes() {
        let dir = test_dir("dir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o722)).unwrap();
        assert!(ensure_dir(&dir, false).is_err());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn managed_parent_rejects_symlinked_ancestor() {
        let home = test_home_dir("symlink");
        let redirected = test_dir("redirect");
        std::os::unix::fs::symlink(&redirected, home.join("Library")).unwrap();
        let paths = paths_from_home(&home).unwrap();

        assert!(ensure_managed_dirs(&paths).is_err());

        fs::remove_file(home.join("Library")).unwrap();
        fs::remove_dir(home).unwrap();
        fs::remove_dir(redirected).unwrap();
    }

    #[test]
    fn staged_files_preserve_live_contents_until_commit() {
        let dir = test_dir("staging");
        let executable = dir.join("executable");
        let plist = dir.join("plist");
        fs::write(&executable, b"old executable").unwrap();
        fs::write(&plist, b"old plist").unwrap();
        let staged_executable =
            stage_file(&executable, b"new executable", EXECUTABLE_MODE).unwrap();
        let staged_plist = stage_file(&plist, b"new plist", PLIST_MODE).unwrap();
        assert_eq!(fs::read(&executable).unwrap(), b"old executable");
        assert_eq!(fs::read(&plist).unwrap(), b"old plist");

        commit_file(staged_executable, &executable).unwrap();
        commit_file(staged_plist, &plist).unwrap();
        assert!(
            managed_file_matches(
                &executable,
                b"new executable",
                effective_uid(),
                EXECUTABLE_MODE
            )
            .unwrap()
        );
        assert!(managed_file_matches(&plist, b"new plist", effective_uid(), PLIST_MODE).unwrap());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_commit_and_abandoned_staging_leave_no_temporary_files() {
        let dir = test_dir("staging-failure");
        let target = dir.join("target");
        fs::create_dir(&target).unwrap();
        let existing = target.join("existing");
        fs::write(&existing, b"keep").unwrap();
        let staged = stage_file(&target, b"replacement", EXECUTABLE_MODE).unwrap();
        let abandoned = stage_file(&dir.join("plist"), b"plist", PLIST_MODE).unwrap();
        let error = commit_file(staged, &target).unwrap_err();
        assert!(error.to_string().contains("install"));
        drop(abandoned);
        assert_eq!(fs::read(&existing).unwrap(), b"keep");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lifecycle_lock_serializes_mutations() {
        let dir = test_dir("lock");
        let path = dir.join("lifecycle.lock");
        let first = LifecycleLock::acquire_at(&path, Duration::ZERO).unwrap();
        assert!(LifecycleLock::acquire_at(&path, Duration::ZERO).is_err());
        drop(first);
        LifecycleLock::acquire_at(&path, Duration::ZERO).unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
