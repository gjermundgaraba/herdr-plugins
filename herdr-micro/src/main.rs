use anyhow::{Context, Result, bail};
use herdr_client::open_rotating_log;
use herdr_micro::{
    control::{log_file, request_status, request_stop, start_daemon_versioned},
    daemon, doctor, helper_install, setup,
};
use std::{
    env,
    ffi::OsString,
    io,
    os::unix::process::CommandExt,
    process::{Command, ExitCode, Stdio},
    time::Duration,
};

const USAGE: &str = "usage: herdr-micro <start|doctor|status|configure|setup-pi-effort|stop|setup|install-helper|uninstall-helper>";

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
        "doctor" if rest.is_empty() => Ok(doctor::run_doctor()),
        "status" if rest.is_empty() => status(),
        "configure" if rest.is_empty() => {
            println!("{}", setup::configure()?.display());
            Ok(0)
        }
        "setup-pi-effort" if rest.is_empty() => setup_pi_effort(),
        "stop" if rest.is_empty() => stop(),
        "setup" if rest.is_empty() => setup_micro(),
        "install-helper" if rest.is_empty() => {
            helper_install::install()?;
            println!("Installed privileged Codex Micro USB helper");
            Ok(0)
        }
        "uninstall-helper" if rest.is_empty() => {
            helper_install::uninstall()?;
            println!("Uninstalled privileged Codex Micro USB helper");
            Ok(0)
        }
        _ => bail!(USAGE),
    }
}

fn start() -> Result<i32> {
    let root = setup::plugin_root()?;
    let executable = env::current_exe()?;
    let status = start_daemon_versioned(
        env!("CARGO_PKG_VERSION"),
        daemon::DAEMON_PROTOCOL_VERSION,
        || {
            helper_install::verify_installed()?;
            let log =
                open_rotating_log(&log_file()?, 10 << 20, 3).context("open Micro bridge log")?;
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
        // Must outlast a draining daemon's teardown (device close and USB
        // release can block for ~13s); success returns as soon as it is ready.
        Duration::from_secs(15),
    )?;
    println!("{}", serde_json::to_string(&status)?);
    Ok(0)
}

fn status() -> Result<i32> {
    let status = request_status(Duration::from_secs(2))?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    // An error payload (e.g. a stopping bridge) is not a healthy status.
    Ok(i32::from(status.get("error").is_some()))
}

fn stop() -> Result<i32> {
    println!(
        "{}",
        serde_json::to_string_pretty(&request_stop(Duration::from_secs(2))?)?
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
            "Configured AppSense Layer 2 and HID keys; backup: {}",
            backup.display()
        );
    } else {
        println!("Layer 2 is already configured");
    }
    println!(
        "Firmware {}: compatible OAI keymap detected",
        report.firmware
    );
    println!("Config: {}", report.config.display());
    Ok(0)
}
