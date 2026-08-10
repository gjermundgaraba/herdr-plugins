use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::{EventEnvelope, PluginInvocationContext};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Environment {
    pub is_herdr: bool,
    pub socket_path: Option<PathBuf>,
    pub bin_path: Option<PathBuf>,
    pub plugin_id: Option<String>,
    pub plugin_root: Option<PathBuf>,
    pub plugin_config_dir: Option<PathBuf>,
    pub plugin_state_dir: Option<PathBuf>,
    pub context: Option<PluginInvocationContext>,
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub pane_id: Option<String>,
    pub action_id: Option<String>,
    pub event_name: Option<String>,
    pub event: Option<EventEnvelope>,
    pub entrypoint_id: Option<String>,
    pub clicked_url: Option<String>,
    pub link_handler_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PluginInvocation<'a> {
    Action(&'a str),
    Event {
        name: &'a str,
        event: &'a EventEnvelope,
    },
    Pane(&'a str),
    Startup,
}

impl Environment {
    pub fn load() -> Result<Self, EnvironmentError> {
        Ok(Self {
            is_herdr: string_var("HERDR_ENV").as_deref() == Some("1"),
            socket_path: path_var("HERDR_SOCKET_PATH"),
            bin_path: path_var("HERDR_BIN_PATH"),
            plugin_id: string_var("HERDR_PLUGIN_ID"),
            plugin_root: path_var("HERDR_PLUGIN_ROOT"),
            plugin_config_dir: path_var("HERDR_PLUGIN_CONFIG_DIR"),
            plugin_state_dir: path_var("HERDR_PLUGIN_STATE_DIR"),
            context: json_var("HERDR_PLUGIN_CONTEXT_JSON")?,
            workspace_id: string_var("HERDR_WORKSPACE_ID"),
            tab_id: string_var("HERDR_TAB_ID"),
            pane_id: string_var("HERDR_PANE_ID"),
            action_id: string_var("HERDR_PLUGIN_ACTION_ID"),
            event_name: string_var("HERDR_PLUGIN_EVENT"),
            event: json_var("HERDR_PLUGIN_EVENT_JSON")?,
            entrypoint_id: string_var("HERDR_PLUGIN_ENTRYPOINT_ID"),
            clicked_url: string_var("HERDR_PLUGIN_CLICKED_URL"),
            link_handler_id: string_var("HERDR_PLUGIN_LINK_HANDLER_ID"),
        })
    }

    pub fn invocation(&self) -> Option<PluginInvocation<'_>> {
        if let Some(action) = self.action_id.as_deref() {
            Some(PluginInvocation::Action(action))
        } else if self.event_name.as_deref() == Some("startup") {
            Some(PluginInvocation::Startup)
        } else if let (Some(name), Some(event)) = (self.event_name.as_deref(), self.event.as_ref())
        {
            Some(PluginInvocation::Event { name, event })
        } else {
            self.entrypoint_id.as_deref().map(PluginInvocation::Pane)
        }
    }

    pub fn require_plugin(&self) -> Result<PluginPaths, PluginEnvironmentError> {
        if !self.is_herdr {
            return Err(PluginEnvironmentError::NotInHerdr);
        }
        Ok(PluginPaths {
            plugin_id: required(&self.plugin_id, "HERDR_PLUGIN_ID")?.clone(),
            root_dir: required(&self.plugin_root, "HERDR_PLUGIN_ROOT")?.clone(),
            config_dir: required(&self.plugin_config_dir, "HERDR_PLUGIN_CONFIG_DIR")?.clone(),
            state_dir: required(&self.plugin_state_dir, "HERDR_PLUGIN_STATE_DIR")?.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPaths {
    pub plugin_id: String,
    pub root_dir: PathBuf,
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl PluginPaths {
    pub fn config_file(&self, name: impl AsRef<Path>) -> PathBuf {
        self.config_dir.join(name)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.state_dir.join("data")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.state_dir.join("cache")
    }

    pub fn run_dir(&self) -> PathBuf {
        self.state_dir.join("run")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginEnvironmentError {
    NotInHerdr,
    Missing(&'static str),
}

impl fmt::Display for PluginEnvironmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInHerdr => f.write_str("plugin must run under Herdr (HERDR_ENV=1)"),
            Self::Missing(variable) => write!(f, "{variable} is not set"),
        }
    }
}

impl std::error::Error for PluginEnvironmentError {}

fn required<'a, T>(
    value: &'a Option<T>,
    variable: &'static str,
) -> Result<&'a T, PluginEnvironmentError> {
    value
        .as_ref()
        .ok_or(PluginEnvironmentError::Missing(variable))
}

pub fn host_config_path() -> PathBuf {
    resolve_host_config_path(
        std::env::var("HERDR_CONFIG_PATH").ok(),
        std::env::var("XDG_CONFIG_HOME").ok(),
        platform_config_dir(),
    )
}

fn resolve_host_config_path(
    explicit: Option<String>,
    xdg_config_home: Option<String>,
    platform_dir: PathBuf,
) -> PathBuf {
    explicit.map(PathBuf::from).unwrap_or_else(|| {
        xdg_config_home
            .map(PathBuf::from)
            .unwrap_or(platform_dir)
            .join("herdr/config.toml")
    })
}

#[cfg(windows)]
fn platform_config_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(PathBuf::from)
        .or_else(|_| {
            std::env::var("USERPROFILE")
                .map(PathBuf::from)
                .map(|profile| profile.join("AppData/Roaming"))
        })
        .or_else(|_| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .unwrap_or_else(|_| std::env::temp_dir())
}

#[cfg(not(windows))]
fn platform_config_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config"))
        .unwrap_or_else(|_| std::env::temp_dir())
}

/// Open an append-only private log, rotating it when it reaches `max_bytes`.
/// `retained_files` includes the active file.
pub fn open_rotating_log(path: &Path, max_bytes: u64, retained_files: usize) -> io::Result<File> {
    if max_bytes == 0 || retained_files == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "log limits must be nonzero",
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() >= max_bytes) {
        for index in (1..retained_files).rev() {
            let source = if index == 1 {
                path.to_path_buf()
            } else {
                numbered_log(path, index - 1)?
            };
            let target = numbered_log(path, index)?;
            match fs::remove_file(&target) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            match fs::rename(&source, &target) {
                Ok(()) => {
                    retain_log_tail(&target, max_bytes)?;
                    make_private(&target)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        if retained_files == 1 {
            fs::remove_file(path)?;
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    make_private(path)?;
    Ok(file)
}

fn numbered_log(path: &Path, index: usize) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "log path has no file name"))?
        .to_os_string();
    name.push(format!(".{index}"));
    Ok(path.with_file_name(name))
}

fn retain_log_tail(path: &Path, max_bytes: u64) -> io::Result<()> {
    let length = fs::metadata(path)?.len();
    if length <= max_bytes {
        return Ok(());
    }
    let mut source = File::open(path)?;
    source.seek(SeekFrom::Start(length - max_bytes))?;
    let mut name = path.file_name().unwrap().to_os_string();
    name.push(format!(".trim-{}.tmp", std::process::id()));
    let temporary = path.with_file_name(name);
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut target = options.open(&temporary)?;
    io::copy(&mut source.take(max_bytes), &mut target)?;
    target.flush()?;
    drop(target);
    fs::remove_file(path)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(unix)]
fn make_private(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn make_private(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[derive(Debug)]
pub struct EnvironmentError {
    pub variable: &'static str,
    pub source: serde_json::Error,
}

impl fmt::Display for EnvironmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} contains invalid JSON: {}",
            self.variable, self.source
        )
    }
}

impl std::error::Error for EnvironmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn string_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn path_var(name: &str) -> Option<PathBuf> {
    string_var(name).map(PathBuf::from)
}

fn json_var<T: serde::de::DeserializeOwned>(
    name: &'static str,
) -> Result<Option<T>, EnvironmentError> {
    string_var(name)
        .map(|json| {
            serde_json::from_str(&json).map_err(|source| EnvironmentError {
                variable: name,
                source,
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_config_path_precedence() {
        let platform = PathBuf::from("/platform");
        assert_eq!(
            resolve_host_config_path(
                Some("/explicit".into()),
                Some("/xdg".into()),
                platform.clone()
            ),
            PathBuf::from("/explicit")
        );
        assert_eq!(
            resolve_host_config_path(None, Some("/xdg".into()), platform.clone()),
            PathBuf::from("/xdg/herdr/config.toml")
        );
        assert_eq!(
            resolve_host_config_path(None, None, platform),
            PathBuf::from("/platform/herdr/config.toml")
        );
    }

    #[test]
    fn classifies_plugin_invocations() {
        let event = EventEnvelope {
            event: "pane_focused".into(),
            data: serde_json::json!({ "pane_id": "w1:p1" }),
        };
        let mut environment = Environment {
            event_name: Some("pane.focused".into()),
            event: Some(event.clone()),
            ..Default::default()
        };
        assert_eq!(
            environment.invocation(),
            Some(PluginInvocation::Event {
                name: "pane.focused",
                event: &event,
            })
        );
        environment.event_name = Some("startup".into());
        environment.event = None;
        assert_eq!(environment.invocation(), Some(PluginInvocation::Startup));
        environment.event_name = None;
        environment.action_id = Some("open".into());
        assert_eq!(
            environment.invocation(),
            Some(PluginInvocation::Action("open"))
        );
        environment.action_id = None;
        environment.entrypoint_id = Some("palette".into());
        assert_eq!(
            environment.invocation(),
            Some(PluginInvocation::Pane("palette"))
        );
    }

    #[test]
    fn requires_complete_plugin_environment() {
        let mut environment = Environment::default();
        assert_eq!(
            environment.require_plugin(),
            Err(PluginEnvironmentError::NotInHerdr)
        );
        environment.is_herdr = true;
        assert_eq!(
            environment.require_plugin(),
            Err(PluginEnvironmentError::Missing("HERDR_PLUGIN_ID"))
        );
        environment.plugin_id = Some("example.plugin".into());
        environment.plugin_root = Some("/plugin".into());
        environment.plugin_config_dir = Some("/config".into());
        environment.plugin_state_dir = Some("/state".into());
        let paths = environment.require_plugin().unwrap();
        assert_eq!(
            paths.config_file("config.toml"),
            PathBuf::from("/config/config.toml")
        );
        assert_eq!(paths.data_dir(), PathBuf::from("/state/data"));
        assert_eq!(paths.cache_dir(), PathBuf::from("/state/cache"));
        assert_eq!(paths.run_dir(), PathBuf::from("/state/run"));
        assert_eq!(paths.logs_dir(), PathBuf::from("/state/logs"));
    }

    #[test]
    fn rotates_and_caps_private_logs() {
        let dir = std::env::temp_dir().join(format!("herdr-client-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("plugin.log");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, b"abcdef").unwrap();
        let mut file = open_rotating_log(&path, 4, 3).unwrap();
        file.write_all(b"new").unwrap();
        drop(file);
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read(dir.join("plugin.log.1")).unwrap(), b"cdef");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
