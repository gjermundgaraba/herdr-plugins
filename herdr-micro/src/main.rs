use anyhow::{bail, Context, Result};
use herdr_micro::{
    actions::{execute_effort_plan, plan_effort_change},
    config::{config_path, default_effort, load_effort},
    control::{ensure_state_dir, log_file, request_status, request_stop, start_daemon},
    daemon, doctor,
    ghostty::{focused_session, inspect_ghostty, probe_session_terminals},
    herdr::{current_environment, discover_sessions, herdr_bin, run_command},
    setup,
};
use serde_json::{json, Value};
use std::{
    env,
    ffi::OsString,
    fs::OpenOptions,
    io,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::PathBuf,
    process::{Command, ExitCode, Stdio},
    thread,
    time::Duration,
};

const USAGE: &str = "usage: herdr-micro <start|herdr-status|doctor|effort raise|lower|status|configure-controls|setup-pi-effort|stop|setup|probe-sessions [--watch]>";

fn main() -> ExitCode {
    match run(env::args_os().skip(1).collect()) {
        Ok(code) => u8::try_from(code)
            .map(ExitCode::from)
            .unwrap_or(ExitCode::FAILURE),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<OsString>) -> Result<i32> {
    let Some(command) = args.first().and_then(|arg| arg.to_str()) else {
        bail!(USAGE);
    };
    let rest = &args[1..];
    match command {
        "start" if rest.is_empty() => start(),
        "daemon" if rest.is_empty() => {
            daemon::run_daemon()?;
            Ok(0)
        }
        "herdr-status" if rest.is_empty() => setup::forward_herdr_status(),
        "doctor" if rest.is_empty() => Ok(doctor::run_doctor()),
        "effort" => effort(rest),
        "status" if rest.is_empty() => status(),
        "configure-controls" if rest.is_empty() => {
            println!("{}", setup::configure_controls()?.display());
            Ok(0)
        }
        "setup-pi-effort" if rest.is_empty() => setup_pi_effort(),
        "stop" if rest.is_empty() => stop(),
        "setup" if rest.is_empty() => setup_micro(),
        "probe-sessions" => probe_sessions(rest),
        _ => bail!(USAGE),
    }
}

fn plugin_root() -> Result<PathBuf> {
    if let Some(root) = env::var_os("HERDR_PLUGIN_ROOT").filter(|root| !root.is_empty()) {
        return Ok(PathBuf::from(root));
    }
    let executable = env::current_exe()?;
    if executable.parent().and_then(|path| path.file_name()) == Some("bin".as_ref()) {
        return setup::plugin_root_from(None, &executable);
    }
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

fn start() -> Result<i32> {
    let root = plugin_root()?;
    let executable = env::current_exe()?;
    let status = start_daemon(
        || {
            ensure_state_dir()?;
            let log = OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .open(log_file())
                .context("open Micro bridge log")?;
            let stderr = log.try_clone()?;
            let mut command = Command::new(&executable);
            command
                .arg("daemon")
                .current_dir(&root)
                .env("HERDR_PLUGIN_ROOT", &root)
                .stdin(Stdio::null())
                .stdout(Stdio::from(log))
                .stderr(Stdio::from(stderr));
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            command.spawn().context("start Micro bridge")?;
            Ok(())
        },
        Duration::from_secs(5),
    )?;
    println!("{}", serde_json::to_string(&status)?);
    Ok(0)
}

fn status() -> Result<i32> {
    println!(
        "{}",
        serde_json::to_string_pretty(&request_status(Duration::from_secs(2))?)?
    );
    Ok(0)
}

fn stop() -> Result<i32> {
    println!(
        "{}",
        serde_json::to_string_pretty(&request_stop(Duration::from_secs(2))?)?
    );
    Ok(0)
}

fn effort(args: &[OsString]) -> Result<i32> {
    let Some(direction) = args.first().and_then(|arg| arg.to_str()) else {
        bail!("usage: herdr-micro effort <raise|lower>");
    };
    if args.len() != 1 || !matches!(direction, "raise" | "lower") {
        bail!("usage: herdr-micro effort <raise|lower>");
    }
    let context: Value = serde_json::from_str(
        &env::var("HERDR_PLUGIN_CONTEXT_JSON").unwrap_or_else(|_| "{}".into()),
    )
    .context("parse HERDR_PLUGIN_CONTEXT_JSON")?;
    let agent = context
        .get("focused_pane_agent")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let pane_id = context
        .get("focused_pane_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let config = if agent == "codex" {
        load_effort(&config_path("effort.json")).map_err(anyhow::Error::msg)?
    } else {
        default_effort()
    };
    let plan = plan_effort_change(agent, direction, pane_id, &config)?;
    execute_effort_plan(&herdr_bin(), &plan, |bin, args| {
        run_command(bin, args, None).map(|_| ())
    })?;
    println!(
        "{}",
        json!({ "agent": agent, "direction": direction, "paneId": pane_id })
    );
    Ok(0)
}

fn setup_pi_effort() -> Result<i32> {
    let result = setup::setup_pi_effort()?;
    if result.unchanged {
        println!(
            "Pi effort extension is current: {}",
            result.target.display()
        );
    } else {
        println!("Installed Pi effort extension: {}", result.target.display());
    }
    if let Some(backup) = result.backup {
        println!("Previous extension backup: {}", backup.display());
    }
    println!("Run /reload in existing Pi sessions.");
    Ok(0)
}

fn setup_micro() -> Result<i32> {
    let report = setup::setup_micro()?;
    if let Some(backup) = report.backup {
        println!(
            "Cloned the OAI layout and bound AppSense Layer 2; backup: {}",
            backup.display()
        );
    } else {
        println!("Layer 2 is already configured");
    }
    println!(
        "Firmware {}: compatible OAI keymap detected",
        report.firmware
    );
    println!("Controls: {}", report.controls.display());
    println!("Effort: {}", report.effort.display());
    println!("Lighting: {}", report.lighting.display());
    Ok(0)
}

fn probe_sessions(args: &[OsString]) -> Result<i32> {
    let watch = matches!(args, [flag] if flag == "--watch");
    if !args.is_empty() && !watch {
        bail!("usage: herdr-micro probe-sessions [--watch]");
    }
    let mappings = probe_session_terminals(
        &discover_sessions(&current_environment())?,
        &current_environment(),
    )?;
    let state = inspect_ghostty()?;
    let mut previous = focused_session(&mappings, &state);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "mappings": mappings,
            "focusedSession": previous,
            "focusedTerminalId": state.focused_terminal_id,
        }))?
    );
    if watch {
        println!("watching; press Ctrl-C to stop");
        loop {
            let next = focused_session(&mappings, &inspect_ghostty()?);
            if next != previous {
                println!(
                    "{} -> {}",
                    previous.as_deref().unwrap_or("none"),
                    next.as_deref().unwrap_or("none")
                );
                previous = next;
            }
            thread::sleep(Duration::from_millis(250));
        }
    }
    Ok(0)
}
