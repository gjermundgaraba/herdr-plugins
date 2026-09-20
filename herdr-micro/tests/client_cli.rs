use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    process::{Command, Output},
    thread,
};
fn command() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_herdr-micro"));
    c.arg("client");
    c
}
fn exchange(
    args: &[&str],
    response: impl FnOnce(&Value) -> Value + Send + 'static,
) -> (Output, Value) {
    let d = tempfile::Builder::new()
        .prefix("mc-")
        .tempdir_in("/tmp")
        .unwrap();
    let path = d.path().join("frontend.sock");
    let l = UnixListener::bind(&path).unwrap();
    let server = thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        writeln!(
            s,
            "{}",
            json!({"type":"hello","protocol":7,"client_id":"fixture-client"})
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        let r: Value = serde_json::from_str(&line).unwrap();
        writeln!(s, "{}", response(&r)).unwrap();
        r
    });
    let o = command()
        .args(args)
        .env("HERDR_FRONTEND_SOCKET", path)
        .output()
        .unwrap();
    (o, server.join().unwrap())
}
#[test]
fn cli_routes_input_without_pane_target() {
    for (args, field, value) in [
        (
            vec!["input", "text", "literal\ntext"],
            "text",
            json!("literal\ntext"),
        ),
        (
            vec!["input", "keys", "ctrl+a", "enter"],
            "keys",
            json!(["ctrl+a", "enter"]),
        ),
    ] {
        let (o, r) = exchange(
            &args,
            |r| json!({"type":"reply","id":r["id"],"result":{"ok":true}}),
        );
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(r["type"], "input");
        assert_eq!(r[field], value);
        assert_eq!(r["protocol"], 7);
        assert!(r.get("route").is_none());
    }
}
#[test]
fn structured_errors_and_mismatched_responses_are_failures() {
    for mismatch in [false, true] {
        let (o, _) = exchange(&["input", "keys", "enter"], move |r| {
            if mismatch {
                json!({"type":"reply","id":9,"result":{"ok":true}})
            } else {
                json!({"type":"error","id":r["id"],"error":{"code":"stale_route","message":"client changed"}})
            }
        });
        assert!(!o.status.success());
        assert!(o.stdout.is_empty());
    }
}
