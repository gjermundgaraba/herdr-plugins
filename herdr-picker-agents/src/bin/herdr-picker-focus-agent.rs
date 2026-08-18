//! Submit command: focuses the agent pane selected in herdr-picker.

use std::process::ExitCode;

use herdr_picker_sdk::{run, submit};

fn main() -> ExitCode {
    let result = submit("agent.focus", "pane_id", "target");
    run(env!("CARGO_BIN_NAME"), result)
}
