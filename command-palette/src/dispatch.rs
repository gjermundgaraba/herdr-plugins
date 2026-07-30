use std::{
    fs::OpenOptions,
    io::Write,
    process::{Command, ExitCode, Stdio},
};

use herdr_client::{Client, Error};
use serde_json::{json, Value};

use crate::model::Dispatch;

const WORKER_FLAG: &str = "--dispatch-after-popup";

pub fn maybe_run_worker() -> Option<ExitCode> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some(WORKER_FLAG) {
        return None;
    }
    let Some(method) = args.next() else {
        return Some(ExitCode::FAILURE);
    };
    let Some(raw_params) = args.next() else {
        return Some(ExitCode::FAILURE);
    };
    let params: Value = match serde_json::from_str(&raw_params) {
        Ok(params) => params,
        Err(error) => {
            log_error(&format!("invalid dispatch parameters: {error}"));
            return Some(ExitCode::FAILURE);
        }
    };

    let result = Client::from_env().and_then(|client| {
        match client.call_value("popup.close", &json!({})) {
            Ok(_) => {}
            Err(Error::Api(error)) if error.code == "popup_not_open" => {}
            Err(error) => return Err(error),
        }
        client.call_value(&method, &params)
    });
    match result {
        Ok(_) => Some(ExitCode::SUCCESS),
        Err(error) => {
            log_error(&format!("{method} failed: {error}"));
            Some(ExitCode::FAILURE)
        }
    }
}

pub fn schedule(dispatch: &Dispatch) -> Result<(), String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("cannot resolve executable: {error}"))?;
    let mut command = Command::new(executable);
    command
        .arg(WORKER_FLAG)
        .arg(&dispatch.method)
        .arg(dispatch.params.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Herdr terminates the popup's whole process session when it closes.
        // The worker needs its own session to survive long enough to dispatch.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }

    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot schedule action: {error}"))
}

fn log_error(message: &str) {
    let Some(state_dir) = std::env::var_os("HERDR_PLUGIN_STATE_DIR") else {
        return;
    };
    let path = std::path::PathBuf::from(state_dir).join("command-palette.log");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}
