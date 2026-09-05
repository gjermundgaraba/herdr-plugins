mod notify;
mod service;

use std::{
    env,
    io::Write,
    path::PathBuf,
    process::{Command, ExitCode},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use herdr_client::open_rotating_log;
use herdr_hub_client::{HubClient, Model};
use serde_json::{Value, json};

const PLUGIN_ID: &str = "gjermundgaraba.herdr-hub";
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    let serving = args.first() == Some(&"serve");
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("herdr-hub: {error:#}");
            if serving {
                log_error(&format!("{error:#}"));
            }
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[&str]) -> Result<()> {
    match args {
        ["serve"] => serve(),
        ["status"] => status(),
        ["doctor"] => doctor(),
        ["notify", kind] => notify::command(kind),
        ["notify"] => bail!("notify expects startup or event"),
        ["ensure"] => service::ensure(),
        ["install-service"] => service::install(),
        ["uninstall-service"] => service::uninstall(),
        ["dump"] => dump(),
        ["relay"] => herdr_hub::run_relay(service::resolve_herdr),
        ["--version" | "-V"] => {
            println!("herdr-hub {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["--help" | "-h", ..] => {
            print_usage();
            Ok(())
        }
        ["notify", _, extra, ..]
        | [
            "serve" | "status" | "doctor" | "ensure" | "install-service" | "uninstall-service"
            | "dump" | "relay" | "--version" | "-V",
            extra,
            ..,
        ] => bail!("unexpected argument: {extra}"),
        _ => usage(),
    }
}

fn serve() -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    bail!("serve is only available as a macOS user service");

    let herdr = env::var_os("HERDR_BIN_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .context("HERDR_BIN_PATH is required by serve")?;
    if !herdr.is_absolute() {
        bail!("HERDR_BIN_PATH must be absolute")
    }
    herdr_hub::run(&herdr)
}

fn status() -> Result<()> {
    let (_, model) = HubClient::new()
        .subscribe(CLIENT_TIMEOUT)
        .context("hub is unreachable")?;
    let summary = json!({
        "version": model.version,
        "active": model.active,
        "hosts": model.hosts.len(),
        "sessions": model.sessions.len(),
        "connected_sessions": model.sessions.iter().filter(|session| session.connected).count(),
        "agents": model.sessions.iter().map(|session| session.agents.len()).sum::<usize>(),
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn dump() -> Result<()> {
    let (_, model) = HubClient::new()
        .subscribe(CLIENT_TIMEOUT)
        .context("hub is unreachable")?;
    println!("{}", serde_json::to_string_pretty(&model)?);
    Ok(())
}

fn doctor() -> Result<()> {
    let herdr = service::resolve_herdr()?;
    println!("ok herdr: {}", herdr.display());
    check_plugin(&herdr)?;
    println!("ok plugin: {PLUGIN_ID} is linked and enabled");
    let (_, model) = HubClient::new()
        .subscribe(CLIENT_TIMEOUT)
        .context("hub is unreachable")?;
    println!(
        "ok hub: model {} has {} session(s)",
        model.version,
        model.sessions.len()
    );
    require_connected_remote_hosts(&model)?;
    service::doctor()?;
    #[cfg(target_os = "macos")]
    println!("ok service: dev.herdr.hub is loaded from the installed copy");
    for (host, version) in herdr_hub::check_remote_hosts()? {
        println!("ok remote {host}: {version}");
    }
    Ok(())
}

fn require_connected_remote_hosts(model: &Model) -> Result<()> {
    for host in model.hosts.iter().filter(|host| host.key != "local") {
        if !host.connected {
            match &host.error {
                Some(error) => bail!("remote {} is disconnected: {error}", host.key),
                None => bail!("remote {} is disconnected", host.key),
            }
        }
    }
    Ok(())
}

fn check_plugin(herdr: &std::path::Path) -> Result<()> {
    let output = Command::new(herdr)
        .args(["plugin", "list", "--plugin", PLUGIN_ID, "--json"])
        .output()
        .context("run plugin.list")?;
    if !output.status.success() {
        bail!(
            "plugin.list failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    let value: Value = serde_json::from_slice(&output.stdout).context("parse plugin.list")?;
    require_enabled_plugin(&value)
}

fn require_enabled_plugin(value: &Value) -> Result<()> {
    let plugin = value
        .pointer("/result/plugins")
        .and_then(Value::as_array)
        .and_then(|plugins| {
            plugins
                .iter()
                .find(|plugin| plugin.get("plugin_id").and_then(Value::as_str) == Some(PLUGIN_ID))
        })
        .ok_or_else(|| anyhow!("plugin {PLUGIN_ID} is not linked"))?;
    if plugin.get("enabled").and_then(Value::as_bool) != Some(true) {
        bail!("plugin {PLUGIN_ID} is disabled")
    }
    Ok(())
}

fn usage<T>() -> Result<T> {
    print_usage();
    bail!("missing or unknown command")
}

fn print_usage() {
    eprintln!(
        "usage: herdr-hub <serve|relay|status|doctor|notify startup|notify event|ensure|install-service|uninstall-service|dump>"
    );
}

fn log_error(message: &str) {
    let Ok(path) = service::hub_log_path() else {
        return;
    };
    if let Ok(mut file) = open_rotating_log(&path, 10 << 20, 3) {
        let _ = writeln!(file, "{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_hub_client::HostState;

    #[test]
    fn invalid_arguments_are_rejected_before_dispatch() {
        for command in [
            "serve",
            "status",
            "doctor",
            "ensure",
            "install-service",
            "uninstall-service",
            "dump",
            "relay",
            "--version",
            "-V",
        ] {
            assert_eq!(
                run(&[command, "extra", "more"]).unwrap_err().to_string(),
                "unexpected argument: extra"
            );
        }
        assert_eq!(
            run(&["notify", "event", "extra"]).unwrap_err().to_string(),
            "unexpected argument: extra"
        );
        assert_eq!(
            run(&["notify"]).unwrap_err().to_string(),
            "notify expects startup or event"
        );
        for args in [&[][..], &["unknown"][..], &["unknown", "extra"][..]] {
            assert_eq!(
                run(args).unwrap_err().to_string(),
                "missing or unknown command"
            );
        }
        for args in [
            &["--help", "extra"][..],
            &["-h"][..],
            &["--version"][..],
            &["-V"][..],
        ] {
            assert!(run(args).is_ok());
        }
    }

    fn model_with_hosts(hosts: Vec<HostState>) -> Model {
        Model {
            version: 1,
            active: None,
            hosts,
            sessions: Vec::new(),
        }
    }

    #[test]
    fn plugin_check_requires_enabled_matching_entry() {
        let enabled = json!({
            "result": {
                "plugins": [{"plugin_id": PLUGIN_ID, "enabled": true}]
            }
        });
        assert!(require_enabled_plugin(&enabled).is_ok());
        assert!(
            require_enabled_plugin(&json!({
                "result": {
                    "plugins": [{"plugin_id": PLUGIN_ID, "enabled": false}]
                }
            }))
            .is_err()
        );
        assert!(require_enabled_plugin(&json!({"result": {"plugins": []}})).is_err());
    }

    #[test]
    fn remote_hosts_must_be_connected() {
        let connected = HostState {
            key: "workbox".into(),
            connected: true,
            error: None,
        };
        assert!(require_connected_remote_hosts(&model_with_hosts(vec![connected])).is_ok());

        let disconnected = HostState {
            key: "workbox".into(),
            connected: false,
            error: Some("ssh exited".into()),
        };
        assert_eq!(
            require_connected_remote_hosts(&model_with_hosts(vec![disconnected]))
                .unwrap_err()
                .to_string(),
            "remote workbox is disconnected: ssh exited"
        );

        let local = HostState {
            key: "local".into(),
            connected: false,
            error: Some("ignored here".into()),
        };
        assert!(require_connected_remote_hosts(&model_with_hosts(vec![local])).is_ok());
    }
}
