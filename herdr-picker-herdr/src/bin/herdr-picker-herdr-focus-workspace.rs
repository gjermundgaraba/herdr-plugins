//! Submit command: focuses the workspace selected in herdr-picker.

use std::process::ExitCode;

use herdr_picker_herdr::{run, submit};

fn main() -> ExitCode {
    run(env!("CARGO_BIN_NAME"), || {
        submit("workspace.focus", "workspace_id", "workspace_id")
    })
}
