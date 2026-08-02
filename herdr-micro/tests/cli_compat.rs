use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
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

fn executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
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
fn herdr_status_forwards_streams_and_exit_code() {
    let dir = temp_dir("status");
    let fake = dir.join("fake-herdr");
    executable(
        &fake,
        "#!/bin/sh\nprintf 'fixture stdout\\n'\nprintf 'fixture stderr\\n' >&2\nexit 23\n",
    );

    let result = command(&["herdr-status"])
        .env("HERDR_BIN_PATH", &fake)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(23));
    assert_eq!(result.stdout, b"fixture stdout\n");
    assert_eq!(result.stderr, b"fixture stderr\n");

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn status_stop_and_start_use_the_newline_json_socket_without_spawning() {
    let dir = temp_dir("socket");
    let state = dir.join("state");
    fs::create_dir(&state).unwrap();
    let socket = state.join("micro.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        for (expected, response) in [
            ("{\"command\":\"status\"}\n", "{\"fixture\":\"status\"}\n"),
            ("{\"command\":\"stop\"}\n", "{\"stopping\":true}\n"),
            ("{\"command\":\"status\"}\n", "{\"fixture\":\"live\"}\n"),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            assert_eq!(read_request(&mut stream), expected);
            stream.write_all(response.as_bytes()).unwrap();
        }
    });

    for (args, expected) in [
        (vec!["status"], "{\n  \"fixture\": \"status\"\n}\n"),
        (vec!["stop"], "{\n  \"stopping\": true\n}\n"),
        (vec!["start"], "{\"fixture\":\"live\"}\n"),
    ] {
        let result = command(&args)
            .env("HERDR_PLUGIN_STATE_DIR", &state)
            .output()
            .unwrap();
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
fn effort_uses_context_config_and_herdr_environment() {
    let dir = temp_dir("effort");
    let config = dir.join("config");
    fs::create_dir(&config).unwrap();
    fs::write(
        config.join("effort.json"),
        r#"{"codex":{"raise":"ctrl+shift+t","lower":"ctrl+t"}}"#,
    )
    .unwrap();
    let calls = dir.join("calls");
    let fake = dir.join("fake-herdr");
    executable(
        &fake,
        "#!/bin/sh\nprintf 'args=%s\\n' \"$*\" >> \"$CALLS\"\nprintf 'context=%s\\nconfig=%s\\n' \"$HERDR_PLUGIN_CONTEXT_JSON\" \"$HERDR_PLUGIN_CONFIG_DIR\" >> \"$CALLS\"\n",
    );

    for (direction, key) in [("raise", "ctrl+shift+t"), ("lower", "ctrl+t")] {
        let context = r#"{"focused_pane_agent":"codex","focused_pane_id":"w1:p2"}"#;
        let result = command(&["effort", direction])
            .env("CALLS", &calls)
            .env("HERDR_BIN_PATH", &fake)
            .env("HERDR_PLUGIN_CONFIG_DIR", &config)
            .env("HERDR_PLUGIN_CONTEXT_JSON", context)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap(),
            serde_json::json!({ "agent": "codex", "direction": direction, "paneId": "w1:p2" })
        );
        let log = fs::read_to_string(&calls).unwrap();
        assert!(log.contains(&format!("args=pane send-keys w1:p2 {key}\n")));
        assert!(log.contains(&format!("context={context}\nconfig={}\n", config.display())));
        fs::write(&calls, "").unwrap();
    }

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

    let noop = command(&["setup-pi-effort"])
        .current_dir(&outside)
        .env("HERDR_PLUGIN_ROOT", &root)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(noop.status.success());
    assert!(String::from_utf8_lossy(&noop.stdout).contains("Pi effort extension is current:"));

    fs::write(&target, b"old extension\n").unwrap();
    let changed = command(&["setup-pi-effort"])
        .current_dir(&outside)
        .env("HERDR_PLUGIN_ROOT", &root)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(changed.status.success());
    assert_eq!(fs::read(&target).unwrap(), bundled);
    let backups: Vec<_> = fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(Result::unwrap)
        .map(|entry| entry.path())
        .filter(|path| {
            path.to_string_lossy()
                .contains("herdr-micro-effort.ts.bak-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), b"old extension\n");

    fs::remove_dir_all(dir).unwrap();
}
