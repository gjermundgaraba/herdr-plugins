use std::{
    env,
    ffi::{CStr, OsStr},
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

const LABEL: &str = "dev.herdr.hub";

pub(crate) fn resolve_herdr() -> Result<PathBuf> {
    if let Some(path) = env::var_os("HERDR_BIN_PATH").filter(|value| !value.is_empty()) {
        return executable(PathBuf::from(path), "HERDR_BIN_PATH");
    }
    let path = env::var_os("PATH").context("PATH is not set")?;
    for directory in env::split_paths(&path) {
        let candidate = directory.join("herdr");
        if is_executable(&candidate) {
            return executable(candidate, "PATH");
        }
    }
    bail!("cannot find herdr in PATH")
}

fn executable(path: PathBuf, source: &str) -> Result<PathBuf> {
    let path = fs::canonicalize(&path)
        .with_context(|| format!("resolve herdr from {source}: {}", path.display()))?;
    if !is_executable(&path) {
        bail!("herdr from {source} is not executable: {}", path.display())
    }
    Ok(path)
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

pub(crate) fn hub_log_path() -> Result<PathBuf> {
    let path = paths()?.log_dir.join("hub.log");
    fs::create_dir_all(path.parent().expect("hub log has a parent"))
        .with_context(|| format!("create {}", path.parent().unwrap().display()))?;
    Ok(path)
}

#[cfg(target_os = "macos")]
pub(crate) use macos::{doctor, ensure, install, uninstall};

#[cfg(not(target_os = "macos"))]
pub(crate) fn install() -> Result<()> {
    bail!("install-service is only available on macOS")
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn uninstall() -> Result<()> {
    bail!("uninstall-service is only available on macOS")
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn ensure() -> Result<()> {
    crate::notify::startup();
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn doctor() -> Result<()> {
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Paths {
    application_dir: PathBuf,
    executable: PathBuf,
    executable_new: PathBuf,
    plist: PathBuf,
    plist_new: PathBuf,
    log_dir: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
}

fn paths() -> Result<Paths> {
    let home = home_dir()?;
    paths_from_home(&home)
}

fn home_dir() -> Result<PathBuf> {
    // The service manages fixed files under the account owning this process;
    // do not let a hook-controlled HOME redirect installation or removal.
    let uid = unsafe { libc::geteuid() };
    let mut size = 16 * 1024;
    loop {
        // SAFETY: the structure is immediately populated by getpwuid_r.
        let mut entry = unsafe { std::mem::zeroed::<libc::passwd>() };
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0_u8; size];
        // SAFETY: all pointers refer to live writable storage for this call.
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
            return Err(std::io::Error::from_raw_os_error(error)).context("look up home directory");
        }
        if result.is_null() || entry.pw_dir.is_null() {
            bail!("no passwd entry for user ID {uid}")
        }
        // SAFETY: getpwuid_r returned a non-null NUL-terminated pw_dir.
        let home = PathBuf::from(OsStr::from_bytes(unsafe {
            CStr::from_ptr(entry.pw_dir).to_bytes()
        }));
        if !home.is_absolute() {
            bail!("passwd home directory must be absolute")
        }
        return Ok(home);
    }
}

fn paths_from_home(home: &Path) -> Result<Paths> {
    if !home.is_absolute() {
        bail!("HOME must be an absolute path")
    }
    let application_dir = home.join("Library/Application Support").join(LABEL);
    let executable = application_dir.join("herdr-hub");
    let plist = home
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"));
    let log_dir = home.join("Library/Logs/herdr-hub");
    Ok(Paths {
        executable_new: application_dir.join("herdr-hub.new"),
        plist_new: home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist.new")),
        stdout: log_dir.join("stdout.log"),
        stderr: log_dir.join("stderr.log"),
        application_dir,
        executable,
        plist,
        log_dir,
    })
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::{
        fs::{File, OpenOptions, TryLockError},
        io,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
        process::{Child, Command, Output, Stdio},
        thread,
        time::{Duration, Instant},
    };

    const BOOTOUT_TIMEOUT: Duration = Duration::from_secs(15);
    const WAIT_INTERVAL: Duration = Duration::from_millis(50);

    pub(crate) fn install() -> Result<()> {
        require_user()?;
        let _lock = LifecycleLock::acquire()?;
        install_locked()
    }

    fn install_locked() -> Result<()> {
        let paths = paths()?;
        let herdr = resolve_herdr()?;
        prepare_directories(&paths)?;
        install_executable(
            &env::current_exe().context("resolve running herdr-hub")?,
            &paths,
        )?;
        install_plist(
            &render_plist(&paths, &herdr, env::var_os("XDG_CONFIG_HOME"))?,
            &paths,
        )?;
        bootout()?;
        bootstrap(&paths.plist)?;
        Ok(())
    }

    pub(crate) fn ensure() -> Result<()> {
        require_user()?;
        let _lock = LifecycleLock::acquire()?;
        let paths = paths()?;
        let source = env::current_exe().context("resolve running herdr-hub")?;
        let herdr = resolve_herdr()?;
        let expected_plist = render_plist(&paths, &herdr, env::var_os("XDG_CONFIG_HOME"))?;
        let current = files_match(&source, &paths.executable)?
            && fs::read(&paths.plist).is_ok_and(|bytes| bytes == expected_plist.as_bytes());
        if !current || !loaded_from(&paths.executable)? {
            install_locked()?;
        }
        crate::notify::startup();
        Ok(())
    }

    pub(crate) fn uninstall() -> Result<()> {
        require_user()?;
        let _lock = LifecycleLock::acquire()?;
        let paths = paths()?;
        bootout()?;
        for path in [
            &paths.plist_new,
            &paths.plist,
            &paths.executable_new,
            &paths.executable,
        ] {
            remove_file(path)?;
        }
        match fs::remove_dir(&paths.application_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("remove {}", paths.application_dir.display()));
            }
        }
        Ok(())
    }

    pub(crate) fn doctor() -> Result<()> {
        require_user()?;
        let paths = paths()?;
        if !files_match(&env::current_exe()?, &paths.executable)? {
            bail!("installed service binary is missing or differs from this executable")
        }
        if !loaded_from(&paths.executable)? {
            bail!(
                "LaunchAgent {LABEL} is not loaded from {}",
                paths.executable.display()
            )
        }
        Ok(())
    }

    fn prepare_directories(paths: &Paths) -> Result<()> {
        fs::create_dir_all(paths.plist.parent().expect("plist has a parent"))
            .context("create LaunchAgents directory")?;
        for path in [&paths.application_dir, &paths.log_dir] {
            fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("chmod {}", path.display()))?;
        }
        Ok(())
    }

    fn install_executable(source: &Path, paths: &Paths) -> Result<()> {
        remove_file(&paths.executable_new)?;
        fs::copy(source, &paths.executable_new).with_context(|| {
            format!(
                "copy {} to {}",
                source.display(),
                paths.executable_new.display()
            )
        })?;
        fs::set_permissions(&paths.executable_new, fs::Permissions::from_mode(0o750))?;
        fs::rename(&paths.executable_new, &paths.executable).with_context(|| {
            format!(
                "replace installed executable {}",
                paths.executable.display()
            )
        })
    }

    fn install_plist(contents: &str, paths: &Paths) -> Result<()> {
        remove_file(&paths.plist_new)?;
        fs::write(&paths.plist_new, contents)
            .with_context(|| format!("write {}", paths.plist_new.display()))?;
        fs::set_permissions(&paths.plist_new, fs::Permissions::from_mode(0o600))?;
        fs::rename(&paths.plist_new, &paths.plist)
            .with_context(|| format!("replace {}", paths.plist.display()))
    }

    fn files_match(source: &Path, installed: &Path) -> Result<bool> {
        let source = fs::read(source).with_context(|| format!("read {}", source.display()))?;
        let installed = match fs::read(installed) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", installed.display()));
            }
        };
        Ok(installed == source)
    }

    pub(super) fn render_plist(
        paths: &Paths,
        herdr: &Path,
        xdg_config_home: Option<std::ffi::OsString>,
    ) -> Result<String> {
        let xdg_config_home = xdg_config_home.filter(|value| !value.is_empty());
        if xdg_config_home
            .as_ref()
            .is_some_and(|value| !Path::new(value).is_absolute())
        {
            bail!("XDG_CONFIG_HOME must be an absolute path")
        }
        let xdg_environment = xdg_config_home.map_or_else(String::new, |path| {
            format!(
                "    <key>XDG_CONFIG_HOME</key>\n    <string>{}</string>\n",
                xml_escape(Path::new(&path))
            )
        });
        Ok(format!(
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
  <key>EnvironmentVariables</key>
  <dict>
    <key>HERDR_BIN_PATH</key>
    <string>{}</string>
{}
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>
"#,
            xml_escape(&paths.executable),
            xml_escape(herdr),
            xdg_environment,
            xml_escape(&paths.stdout),
            xml_escape(&paths.stderr),
        ))
    }

    fn xml_escape(path: &Path) -> String {
        path.to_string_lossy()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    fn loaded_from(executable: &Path) -> Result<bool> {
        let output = Command::new("/bin/launchctl")
            .args(["print", &service_target()])
            .output()
            .context("inspect Herdr Hub LaunchAgent")?;
        Ok(output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains(&*executable.to_string_lossy()))
    }

    fn bootout() -> Result<()> {
        let child = Command::new("/bin/launchctl")
            .args(["bootout", "--wait", &service_target()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("stop Herdr Hub LaunchAgent")?;
        let output = wait_for_bootout(child, BOOTOUT_TIMEOUT)?;
        if output.status.success() || not_loaded(&output) {
            Ok(())
        } else {
            command_error("stop Herdr Hub LaunchAgent", output)
        }
    }

    fn wait_for_bootout(mut child: Child, timeout: Duration) -> Result<Output> {
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error).context("wait for Herdr Hub LaunchAgent to stop");
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "launchctl bootout timed out after {}s",
                    timeout.as_secs_f64()
                )
            }
            thread::sleep(WAIT_INTERVAL);
        }
        let output = child
            .wait_with_output()
            .context("collect launchctl bootout output")?;
        Ok(output)
    }

    fn bootstrap(plist: &Path) -> Result<()> {
        let output = Command::new("/bin/launchctl")
            .arg("bootstrap")
            .arg(service_domain())
            .arg(plist)
            .output()
            .context("start Herdr Hub LaunchAgent")?;
        if output.status.success() || loaded_from(&paths()?.executable)? {
            Ok(())
        } else {
            command_error("start Herdr Hub LaunchAgent", output)
        }
    }

    fn not_loaded(output: &Output) -> bool {
        let stderr = String::from_utf8_lossy(&output.stderr);
        output.status.code() == Some(3)
            && (stderr.contains("No such process") || stderr.contains("Could not find service"))
    }

    fn command_error(action: &str, output: Output) -> Result<()> {
        bail!(
            "{action} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }

    fn service_domain() -> String {
        format!("gui/{}", effective_uid())
    }

    fn service_target() -> String {
        format!("{}/{LABEL}", service_domain())
    }

    fn require_user() -> Result<()> {
        if effective_uid() == 0 {
            bail!("Herdr Hub service commands refuse to run as root")
        }
        Ok(())
    }

    fn effective_uid() -> libc::uid_t {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() }
    }

    struct LifecycleLock(#[allow(dead_code)] File);

    impl LifecycleLock {
        fn acquire() -> Result<Self> {
            let path = PathBuf::from(format!("/tmp/herdr-hub-{}.lifecycle.lock", effective_uid()));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
                .with_context(|| format!("open lifecycle lock {}", path.display()))?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.uid() != effective_uid()
                || metadata.nlink() != 1
                || metadata.mode() & 0o7777 != 0o600
            {
                bail!("unsafe lifecycle lock {}", path.display())
            }
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                match file.try_lock() {
                    Ok(()) => return Ok(Self(file)),
                    Err(TryLockError::WouldBlock) => {}
                    Err(TryLockError::Error(error)) => {
                        return Err(error).context("lock Herdr Hub lifecycle");
                    }
                }
                if Instant::now() >= deadline {
                    bail!("another Herdr Hub lifecycle command is running")
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    fn remove_file(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
        }
    }

    #[cfg(test)]
    mod tests {
        use std::time::{SystemTime, UNIX_EPOCH};

        use super::*;

        #[test]
        fn executable_comparison_detects_rebuilt_input() {
            let directory = std::env::temp_dir().join(format!(
                "herdr-hub-service-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&directory).unwrap();
            let source = directory.join("source");
            let installed = directory.join("installed");
            fs::write(&source, b"first build").unwrap();

            assert!(!files_match(&source, &installed).unwrap());
            fs::copy(&source, &installed).unwrap();
            assert!(files_match(&source, &installed).unwrap());

            fs::write(&source, b"second build").unwrap();
            assert!(!files_match(&source, &installed).unwrap());
            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn bootout_timeout_kills_and_reaps_promptly() {
            let child = Command::new("/bin/sh")
                .args(["-c", "exec sleep 5"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let started = Instant::now();

            let error = wait_for_bootout(child, Duration::from_millis(10)).unwrap_err();

            assert!(error.to_string().contains("timed out after 0.01s"));
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_paths_are_installed_copies() {
        let paths = paths_from_home(Path::new("/Users/test")).unwrap();
        assert_eq!(
            paths.executable,
            Path::new("/Users/test/Library/Application Support/dev.herdr.hub/herdr-hub")
        );
        assert_eq!(
            paths.plist,
            Path::new("/Users/test/Library/LaunchAgents/dev.herdr.hub.plist")
        );
        assert_eq!(
            paths.log_dir,
            Path::new("/Users/test/Library/Logs/herdr-hub")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_has_the_service_contract() {
        let paths = paths_from_home(Path::new("/Users/a&b")).unwrap();
        let plist = macos::render_plist(
            &paths,
            Path::new("/opt/herdr"),
            Some("/Users/a&b/.config".into()),
        )
        .unwrap();
        assert!(plist.contains("<string>dev.herdr.hub</string>"));
        assert!(plist.contains("<string>serve</string>"));
        assert!(plist.contains("<key>HERDR_BIN_PATH</key>"));
        assert!(plist.contains("<string>/opt/herdr</string>"));
        assert!(plist.contains("<key>XDG_CONFIG_HOME</key>"));
        assert!(plist.contains("<string>/Users/a&amp;b/.config</string>"));
        assert!(plist.contains("/Users/a&amp;b/Library/Application Support"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_omits_empty_xdg_config_home_and_rejects_relative_paths() {
        let paths = paths_from_home(Path::new("/Users/test")).unwrap();
        for value in [None, Some("".into())] {
            let plist = macos::render_plist(&paths, Path::new("/opt/herdr"), value).unwrap();
            assert!(!plist.contains("XDG_CONFIG_HOME"));
        }
        assert!(
            macos::render_plist(&paths, Path::new("/opt/herdr"), Some("relative".into())).is_err()
        );
    }
}
