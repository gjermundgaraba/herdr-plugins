use anyhow::{Context, Result, bail};
use herdr_micro::{
    control::{request_status, request_stop, start_daemon_versioned},
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
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<OsString>) -> Result<ExitCode> {
    let Some(command) = args.first().and_then(|arg| arg.to_str()) else {
        bail!(USAGE);
    };
    let rest = &args[1..];
    match command {
        "start" if rest.is_empty() => start(),
        "daemon" if rest.is_empty() => {
            std::panic::set_hook(Box::new(|panic| {
                daemon::log(format!("bridge panicked: {panic}"));
            }));
            if let Err(error) = daemon::run_daemon() {
                daemon::log(format!("bridge failed: {error:#}"));
                return Err(error);
            }
            Ok(ExitCode::SUCCESS)
        }
        "doctor" if rest.is_empty() => Ok(ExitCode::from(doctor::run_doctor() as u8)),
        "status" if rest.is_empty() => status(),
        "configure" if rest.is_empty() => {
            println!("{}", setup::configure()?.display());
            Ok(ExitCode::SUCCESS)
        }
        "setup-pi-effort" if rest.is_empty() => setup_pi_effort(),
        "stop" if rest.is_empty() => stop(),
        "setup" if rest.is_empty() => setup_micro(),
        "install-helper" if rest.is_empty() => {
            helper_install::install()?;
            println!("Installed privileged Codex Micro USB helper");
            Ok(ExitCode::SUCCESS)
        }
        "uninstall-helper" if rest.is_empty() => {
            helper_install::uninstall()?;
            println!("Uninstalled privileged Codex Micro USB helper");
            Ok(ExitCode::SUCCESS)
        }
        _ => bail!(USAGE),
    }
}

fn start() -> Result<ExitCode> {
    let root = setup::plugin_root()?;
    let executable = env::current_exe()?;
    let status = start_daemon_versioned(
        env!("CARGO_PKG_VERSION"),
        daemon::DAEMON_PROTOCOL_VERSION,
        || {
            helper_install::verify_installed()?;
            let mut command = Command::new(&executable);
            command
                .arg("daemon")
                .current_dir(&root)
                .env("HERDR_PLUGIN_ROOT", &root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
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
    Ok(ExitCode::SUCCESS)
}

fn status() -> Result<ExitCode> {
    let status = request_status(Duration::from_secs(2))?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    // An error payload (e.g. a stopping bridge) is not a healthy status.
    Ok(ExitCode::from(status.get("error").is_some() as u8))
}

fn stop() -> Result<ExitCode> {
    println!(
        "{}",
        serde_json::to_string_pretty(&request_stop(Duration::from_secs(2))?)?
    );
    Ok(ExitCode::SUCCESS)
}

fn setup_pi_effort() -> Result<ExitCode> {
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
    Ok(ExitCode::SUCCESS)
}

fn setup_micro() -> Result<ExitCode> {
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
    Ok(ExitCode::SUCCESS)
}
