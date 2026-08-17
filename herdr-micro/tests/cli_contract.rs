use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

const BIN: &str = env!("CARGO_BIN_EXE_herdr-micro");

fn temp_dir(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = env::temp_dir().join(format!(
        "herdr-micro-cli-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn command(args: &[&str]) -> Command {
    let mut command = Command::new(BIN);
    command.args(args);
    command
}

fn output(args: &[&str]) -> Output {
    command(args).output().unwrap()
}

fn plugin_command(args: &[&str], dir: &Path, state: &Path) -> Command {
    let mut command = command(args);
    command
        .env("HERDR_ENV", "1")
        .env("HERDR_PLUGIN_ID", "example.micro")
        .env("HERDR_PLUGIN_ROOT", dir.join("plugin"))
        .env("HERDR_PLUGIN_CONFIG_DIR", dir.join("config"))
        .env("HERDR_PLUGIN_STATE_DIR", state);
    command
}

fn read_request(stream: &mut UnixStream) -> String {
    let mut request = String::new();
    BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut request)
        .unwrap();
    request
}

#[test]
fn missing_and_invalid_cli_are_useful_errors() {
    for args in [Vec::new(), vec!["missing-command"]] {
        let result = output(&args);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("usage: herdr-micro"));
    }
}

#[test]
fn status_stop_and_start_use_the_newline_json_socket_without_spawning() {
    let dir = temp_dir("socket");
    let state = dir.join("state");
    let run = state.join("run");
    fs::create_dir_all(&run).unwrap();
    let socket = run.join("micro.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        for (expected, response) in [
            ("{\"command\":\"status\"}\n", "{\"fixture\":\"status\"}\n"),
            ("{\"command\":\"stop\"}\n", "{\"stopping\":true}\n"),
            (
                "{\"command\":\"status\"}\n",
                concat!(
                    "{\"fixture\":\"live\",\"version\":\"",
                    env!("CARGO_PKG_VERSION"),
                    "\",\"protocol\":2}\n"
                ),
            ),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            assert_eq!(read_request(&mut stream), expected);
            stream.write_all(response.as_bytes()).unwrap();
        }
    });

    for (args, expected) in [
        (vec!["status"], "{\n  \"fixture\": \"status\"\n}\n"),
        (vec!["stop"], "{\n  \"stopping\": true\n}\n"),
        (
            vec!["start"],
            concat!(
                "{\"fixture\":\"live\",\"protocol\":2,\"version\":\"",
                env!("CARGO_PKG_VERSION"),
                "\"}\n"
            ),
        ),
    ] {
        let result = plugin_command(&args, &dir, &state).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(String::from_utf8(result.stdout).unwrap(), expected);
    }
    server.join().unwrap();

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn setup_pi_effort_uses_plugin_root_outside_the_repository() {
    let dir = temp_dir("pi-effort");
    let root = dir.join("plugin");
    let source = root.join("integrations/pi/herdr-effort.js");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let bundled =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("integrations/pi/herdr-effort.js"))
            .unwrap();
    fs::write(&source, &bundled).unwrap();
    let home = dir.join("home");
    let outside = dir.join("outside");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&outside).unwrap();
    let target = home.join(".pi/agent/extensions/herdr-micro-effort.ts");

    let install = command(&["setup-pi-effort"])
        .current_dir(&outside)
        .env("HERDR_PLUGIN_ROOT", &root)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(install.status.success());
    assert_eq!(fs::read(&target).unwrap(), bundled);
    assert!(String::from_utf8_lossy(&install.stdout).contains("Installed Pi effort extension:"));

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn thinking_effort_adapter_uses_frozen_target_and_herdr_binary() {
    let dir = temp_dir("thinking-effort");
    let fake_herdr = dir.join("herdr");
    let log = dir.join("calls");
    fs::write(
        &fake_herdr,
        "#!/bin/sh\nprintf '%s\\n' --call \"$@\" >> \"$HERDR_TEST_LOG\"\n",
    )
    .unwrap();
    fs::set_permissions(&fake_herdr, fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR")).join("integrations/thinking-effort.sh");

    let result = Command::new("/bin/sh")
        .arg(&adapter)
        .arg("ctrl+shift+t")
        .env("HERDR_BIN_PATH", &fake_herdr)
        .env("HERDR_PANE_ID", "w9:p4")
        .env("HERDR_MICRO_REPEAT", "3")
        .env("HERDR_TEST_LOG", &log)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "--call\npane\nsend-keys\nw9:p4\nctrl+shift+t\nctrl+shift+t\nctrl+shift+t\n"
    );

    fs::remove_file(&log).unwrap();
    let result = Command::new("/bin/sh")
        .arg(&adapter)
        .args(["claude", "lower"])
        .env("HERDR_BIN_PATH", &fake_herdr)
        .env("HERDR_PANE_ID", "w9:p4")
        .env("HERDR_MICRO_REPEAT", "3")
        .env("HERDR_TEST_LOG", &log)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "--call\npane\nsend-text\nw9:p4\n/effort\n--call\npane\nsend-keys\nw9:p4\nenter\n--call\npane\nsend-keys\nw9:p4\nleft\nleft\nleft\n--call\npane\nsend-keys\nw9:p4\nenter\n"
    );
    fs::remove_dir_all(dir).unwrap();
}
