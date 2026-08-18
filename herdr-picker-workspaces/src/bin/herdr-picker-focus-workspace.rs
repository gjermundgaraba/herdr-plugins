//! Submit command: focuses the workspace selected in herdr-picker.

use std::process::ExitCode;

use herdr_picker_sdk::{run, submit};

fn main() -> ExitCode {
    let result = submit("workspace.focus", "workspace_id", "workspace_id");
    run(env!("CARGO_BIN_NAME"), result)
}
