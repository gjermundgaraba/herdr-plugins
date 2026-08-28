use std::process::Command;

#[test]
fn uninstall_rejects_trailing_arguments_before_dispatch() {
    let output = Command::new(env!("CARGO_BIN_EXE_codex-micro"))
        .args(["uninstall", "extra"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage: codex-micro"));
}
