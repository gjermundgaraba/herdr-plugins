use herdr_client::{Environment, PluginInvocationContext, host_config_path};
use serde::Deserialize;
use std::{
    env, fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

#[derive(Default, Deserialize)]
#[serde(default)]
struct HerdrConfig {
    terminal: TerminalConfig,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct TerminalConfig {
    default_shell: String,
    shell_mode: ShellMode,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ShellMode {
    #[default]
    Auto,
    Login,
    NonLogin,
}

fn main() -> ExitCode {
    match start_shell() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("popup-terminal: {error}");
            ExitCode::FAILURE
        }
    }
}

fn start_shell() -> Result<(), String> {
    let context = Environment::load()
        .map_err(|error| error.to_string())?
        .context
        .ok_or_else(|| "HERDR_PLUGIN_CONTEXT_JSON is missing".to_string())?;
    let cwd = invocation_cwd(context)?;
    let (shell, mode) = load_shell_settings(&host_config_path())?;
    env::set_current_dir(&cwd).map_err(|error| format!("change directory to {cwd:?}: {error}"))?;

    let shell_path = find_shell(&shell)?;
    let login = uses_login_shell(mode, env::consts::OS);
    let mut command = Command::new(&shell_path);
    command.arg0(if login {
        format!("-{}", shell_path.file_name().unwrap().to_string_lossy())
    } else {
        shell
    });
    if login {
        command.env("SHELL", &shell_path);
    }
    Err(format!(
        "start shell {}: {}",
        shell_path.display(),
        command.exec()
    ))
}

fn invocation_cwd(context: PluginInvocationContext) -> Result<String, String> {
    [context.focused_pane_cwd, context.workspace_cwd]
        .into_iter()
        .flatten()
        .find(|cwd| !cwd.is_empty())
        .ok_or_else(|| "no focused pane or workspace working directory".into())
}

fn load_shell_settings(path: &Path) -> Result<(String, ShellMode), String> {
    let config: HerdrConfig = match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents)
            .map_err(|error| format!("parse {}: {error}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HerdrConfig::default(),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let shell = match config.terminal.default_shell.trim() {
        "" => env::var("SHELL").unwrap_or_default().trim().to_owned(),
        shell => shell.to_owned(),
    };
    Ok((
        if shell.is_empty() {
            "/bin/sh".into()
        } else {
            shell
        },
        config.terminal.shell_mode,
    ))
}

fn find_shell(shell: &str) -> Result<PathBuf, String> {
    let path = Path::new(shell);
    if shell.contains('/') {
        return is_executable(path)
            .then(|| path.to_owned())
            .ok_or_else(|| format!("find shell {shell:?}: not an executable file"));
    }
    let search_path = env::var_os("PATH").unwrap_or_default();
    env::split_paths(&search_path)
        .map(|directory| directory.join(shell))
        .find(|path| is_executable(path))
        .ok_or_else(|| format!("find shell {shell:?}: executable file not found in PATH"))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn uses_login_shell(mode: ShellMode, os: &str) -> bool {
    match mode {
        ShellMode::Auto => os == "macos",
        ShellMode::Login => true,
        ShellMode::NonLogin => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_ported_behavior() {
        assert_eq!(
            invocation_cwd(PluginInvocationContext {
                focused_pane_cwd: Some("/focused".into()),
                workspace_cwd: Some("/workspace".into()),
                ..Default::default()
            })
            .unwrap(),
            "/focused"
        );
        assert_eq!(
            invocation_cwd(PluginInvocationContext {
                workspace_cwd: Some("/workspace".into()),
                ..Default::default()
            })
            .unwrap(),
            "/workspace"
        );
        let path = env::temp_dir().join(format!("popup-terminal-{}.toml", std::process::id()));
        fs::write(
            &path,
            "[terminal]\ndefault_shell = 'fish'\nshell_mode = 'non_login'\n",
        )
        .unwrap();
        let settings = load_shell_settings(&path).unwrap();
        assert_eq!(settings, ("fish".into(), ShellMode::NonLogin));
        fs::write(&path, "[terminal]\nshell_mode = 'invalid'\n").unwrap();
        assert!(load_shell_settings(&path).is_err());
        fs::remove_file(path).unwrap();
        assert!(uses_login_shell(ShellMode::Auto, "macos"));
        assert!(!uses_login_shell(ShellMode::Auto, "linux"));
    }
}
