use std::{
    env,
    ffi::OsStr,
    fs,
    io::{self, BufReader, Read, Write},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, ExitCode, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use crate::wire::ProviderMessage;
use anyhow::{Context, Result, anyhow, bail};
use herdr_client::{Client, Error, ndjson, open_rotating_log};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{Item, validate_items};

const WORKER_FLAG: &str = "--submit-after-popup";
const MAX_STDERR_BYTES: usize = 64 << 10;

#[derive(Debug)]
pub enum ProviderEvent {
    Snapshot(Vec<Item>),
    Unavailable(String),
    Error(String),
    Exited { status: ExitStatus, stderr: String },
}

pub struct Provider {
    events: Option<mpsc::Receiver<ProviderEvent>>,
    stop: mpsc::Sender<()>,
    supervisor: Option<thread::JoinHandle<()>>,
}

impl Provider {
    pub fn start(argv: &[String], context: Value) -> Result<Self> {
        let Some(program) = argv.first() else {
            bail!("source argv must not be empty");
        };
        let mut encoded = serde_json::to_vec(&context).context("cannot encode picker context")?;
        encoded.push(b'\n');
        let mut child = Command::new(program)
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .with_context(|| format!("cannot start {}", display(argv)))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let child_stderr = child.stderr.take().expect("piped");

        let (event_tx, event_rx) = mpsc::sync_channel(1);
        let supervisor_events = event_tx.clone();
        let (fatal_tx, fatal_rx) = mpsc::channel();
        let writer_fatal = fatal_tx.clone();
        let command_name = display(argv);
        let writer = thread::spawn(move || {
            let mut stdin = stdin;
            if let Err(error) = stdin.write_all(&encoded)
                && error.kind() != io::ErrorKind::BrokenPipe
            {
                let _ = writer_fatal.send(format!(
                    "cannot pass picker context to {command_name}: {error}"
                ));
            }
        });

        let output_reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut pending = Vec::new();
            loop {
                match ndjson::read_frame(&mut reader, &mut pending) {
                    Ok(Some(frame)) => match serde_json::from_slice::<ProviderMessage>(&frame) {
                        Ok(ProviderMessage::Snapshot(snapshot)) => {
                            let items = snapshot.items;
                            if let Err(error) = validate_items(&items) {
                                let _ = fatal_tx.send(format!("provider items: {error}"));
                                return;
                            }
                            if event_tx.send(ProviderEvent::Snapshot(items)).is_err() {
                                return;
                            }
                        }
                        Ok(ProviderMessage::Error { error }) => {
                            if event_tx.send(ProviderEvent::Unavailable(error)).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = fatal_tx
                                .send(format!("provider returned invalid message: {error}"));
                            return;
                        }
                    },
                    Ok(None) => return,
                    Err(error) => {
                        let _ = fatal_tx.send(format!("cannot read provider output: {error}"));
                        return;
                    }
                }
            }
        });

        let stderr_reader = thread::spawn(move || capture_tail(child_stderr));
        let (stop_tx, stop_rx) = mpsc::channel();
        let supervisor = thread::spawn(move || {
            supervise_provider(
                &mut child,
                stop_rx,
                fatal_rx,
                writer,
                output_reader,
                stderr_reader,
                supervisor_events,
            );
        });

        Ok(Self {
            events: Some(event_rx),
            stop: stop_tx,
            supervisor: Some(supervisor),
        })
    }

    pub fn try_recv(&self) -> Result<ProviderEvent, mpsc::TryRecvError> {
        self.events.as_ref().unwrap().try_recv()
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.events.take();
        let _ = self.stop.send(());
        if let Some(supervisor) = self.supervisor.take() {
            let _ = supervisor.join();
        }
    }
}

fn supervise_provider(
    child: &mut Child,
    stop: mpsc::Receiver<()>,
    fatal: mpsc::Receiver<String>,
    writer: thread::JoinHandle<()>,
    output_reader: thread::JoinHandle<()>,
    stderr_reader: thread::JoinHandle<Vec<u8>>,
    events: mpsc::SyncSender<ProviderEvent>,
) {
    let mut stopped = false;
    let mut failure = None;
    let status = loop {
        match stop.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                stopped = true;
                kill_provider_processes(child);
                break child.wait();
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if let Ok(error) = fatal.try_recv() {
            failure = Some(error);
            kill_provider_processes(child);
            break child.wait();
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                kill_provider_processes(child);
                break Err(error);
            }
        }
    };

    kill_provider_processes(child);
    let writer_result = writer.join();
    let output_result = output_reader.join();
    let stderr = stderr_reader.join().unwrap_or_default();
    if stopped {
        return;
    }
    if writer_result.is_err() || output_result.is_err() {
        let _ = events.send(ProviderEvent::Error("provider I/O thread panicked".into()));
        return;
    }
    if let Some(error) = failure.or_else(|| fatal.try_iter().next()) {
        let stderr = String::from_utf8_lossy(&stderr);
        let stderr = stderr.trim();
        let _ = events.send(ProviderEvent::Error(if stderr.is_empty() {
            error
        } else {
            format!("{error}\n{stderr}")
        }));
        return;
    }
    match status {
        Ok(status) => {
            let _ = events.send(ProviderEvent::Exited {
                status,
                stderr: String::from_utf8_lossy(&stderr).trim().to_owned(),
            });
        }
        Err(error) => {
            let _ = events.send(ProviderEvent::Error(format!(
                "cannot wait for provider: {error}"
            )));
        }
    }
}

fn kill_provider_processes(child: &mut Child) {
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.kill();
}

#[derive(Deserialize, Serialize)]
struct SubmitRequest {
    command: Vec<String>,
    input: Value,
}

pub fn maybe_run_submit_worker() -> Option<ExitCode> {
    if env::args_os().nth(1).as_deref() != Some(OsStr::new(WORKER_FLAG)) {
        return None;
    }
    let result = run_submit_worker();
    if let Err(error) = &result {
        let message = format!("{error:#}");
        log_error(&message);
        notify_failure(&message);
    }
    Some(if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

pub fn running_in_popup() -> bool {
    popup_environment(
        env::var_os("HERDR_ENV").as_deref(),
        env::var_os("HERDR_PANE_ID").as_deref(),
        env::var_os("HERDR_ACTIVE_PANE_ID").as_deref(),
    )
}

fn popup_environment(
    herdr: Option<&OsStr>,
    pane: Option<&OsStr>,
    active_pane: Option<&OsStr>,
) -> bool {
    herdr == Some(OsStr::new("1")) && pane.is_none() && active_pane.is_some()
}

pub fn submit(argv: &[String], input: &Value) -> Result<()> {
    if running_in_popup() {
        schedule_submit(argv, input)
    } else {
        run(argv, input)
    }
}

fn schedule_submit(argv: &[String], input: &Value) -> Result<()> {
    let executable = env::current_exe().context("cannot resolve executable")?;
    let mut command = Command::new(executable);
    command
        .arg(WORKER_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // The popup closes its process session; the submit worker must survive it.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }

    let mut child = command.spawn().context("cannot schedule submit command")?;
    let request = SubmitRequest {
        command: argv.to_vec(),
        input: input.clone(),
    };
    let mut stdin = child.stdin.take().expect("piped");
    serde_json::to_writer(&mut stdin, &request).context("cannot encode submit request")?;
    stdin.write_all(b"\n").context("cannot send submit request")
}

fn run_submit_worker() -> Result<()> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("cannot read submit request")?;
    let request: SubmitRequest = serde_json::from_str(&input).context("invalid submit request")?;

    if running_in_popup() {
        let client = Client::from_env().context("cannot connect to Herdr")?;
        match client.call_value("popup.close", &json!({})) {
            Ok(_) => {}
            Err(Error::Api(error)) if error.code == "popup_not_open" => {}
            Err(error) => return Err(error).context("cannot close picker popup"),
        }
    }
    run(&request.command, &request.input)
}

fn run(argv: &[String], input: &Value) -> Result<()> {
    let Some(program) = argv.first() else {
        bail!("command argv must not be empty");
    };
    let mut encoded = serde_json::to_vec(input).context("cannot encode picker context")?;
    encoded.push(b'\n');
    let mut child = Command::new(program)
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot start {}", display(argv)))?;
    let stderr = child.stderr.take().expect("piped");
    let stdin = child.stdin.take().expect("piped");
    if let Err(error) = set_nonblocking(&stdin).and_then(|()| set_nonblocking(&stderr)) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error).context("cannot configure command pipes");
    }

    let stopped = Arc::new(AtomicBool::new(false));
    let writer_stopped = Arc::clone(&stopped);
    let writer = thread::spawn(move || write_until_stopped(stdin, &encoded, &writer_stopped));
    let reader_stopped = Arc::clone(&stopped);
    let stderr_reader = thread::spawn(move || capture_tail_until(stderr, &reader_stopped));
    let status_result = child
        .wait()
        .with_context(|| format!("cannot wait for {}", display(argv)));
    if status_result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    stopped.store(true, Ordering::Relaxed);
    let write_result = writer
        .join()
        .map_err(|_| anyhow!("input writer for {} panicked", display(argv)))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("stderr reader for {} panicked", display(argv)))?;

    if let Err(error) = write_result
        && error.kind() != io::ErrorKind::BrokenPipe
    {
        return Err(error)
            .with_context(|| format!("cannot pass picker context to {}", display(argv)));
    }
    let status = status_result?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let stderr = stderr.trim();
        bail!(if stderr.is_empty() {
            format!("{} exited with {status}", display(argv))
        } else {
            format!("{} failed: {stderr}", display(argv))
        });
    }
    Ok(())
}

fn capture_tail(mut reader: impl Read) -> Vec<u8> {
    let mut captured = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => extend_tail(&mut captured, &chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    captured
}

fn capture_tail_until(mut reader: impl Read, stopped: &AtomicBool) -> Vec<u8> {
    let mut captured = Vec::new();
    let mut chunk = [0; 4096];
    let mut read_after_stop = 0;
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                extend_tail(&mut captured, &chunk[..count]);
                if stopped.load(Ordering::Relaxed) {
                    read_after_stop += count;
                    if read_after_stop >= MAX_STDERR_BYTES {
                        break;
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    captured
}

fn write_until_stopped(
    mut writer: impl Write,
    mut input: &[u8],
    stopped: &AtomicBool,
) -> io::Result<()> {
    while !input.is_empty() {
        if stopped.load(Ordering::Relaxed) {
            return Ok(());
        }
        match writer.write(input) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => input = &input[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn extend_tail(captured: &mut Vec<u8>, bytes: &[u8]) {
    captured.extend_from_slice(bytes);
    if captured.len() > MAX_STDERR_BYTES {
        let excess = captured.len() - MAX_STDERR_BYTES;
        captured.drain(..excess);
    }
}

fn set_nonblocking(stream: &impl std::os::fd::AsRawFd) -> io::Result<()> {
    let descriptor = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags == -1
        || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn display(argv: &[String]) -> String {
    argv.join(" ")
}

fn log_error(message: &str) {
    let Some(path) = log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = open_rotating_log(&path, 1 << 20, 3) {
        let _ = writeln!(file, "{message}");
    }
}

fn notify_failure(message: &str) {
    let Ok(client) = Client::from_env() else {
        return;
    };
    let _ = client.call_value(
        "notification.show",
        &json!({
            "title": "Picker command failed",
            "body": message,
        }),
    );
}

fn log_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("XDG_STATE_HOME").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path).join("herdr-picker").join("picker.log"));
    }
    env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|path| {
            path.join(".local")
                .join("state")
                .join("herdr-picker")
                .join("picker.log")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn popup_environment_requires_herdr_without_a_pane_identity() {
        let one = OsStr::new("1");
        let pane = OsStr::new("w1:p1");

        assert!(popup_environment(Some(one), None, Some(pane)));
        assert!(!popup_environment(Some(one), Some(pane), Some(pane)));
        assert!(!popup_environment(None, None, Some(pane)));
        assert!(!popup_environment(Some(one), None, None));
    }

    #[test]
    fn provider_receives_context_and_streams_snapshots() {
        let argv = vec![
            "sh".into(),
            "-c".into(),
            r#"read input
case "$input" in *'"selected":"model"'*) ;; *) exit 2;; esac
printf '{"items":[{"id":"one","title":"One"}]}\n'"#
                .into(),
        ];
        let provider = Provider::start(&argv, json!({ "selected": "model" })).unwrap();
        let event = provider
            .events
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            event,
            ProviderEvent::Snapshot(items) if items[0].id == "one"
        ));
    }

    #[test]
    fn provider_data_errors_include_stderr() {
        let provider = Provider::start(
            &[
                "sh".into(),
                "-c".into(),
                concat!(
                    "cat >/dev/null; echo source diagnostic >&2; ",
                    "printf '{\"items\":[{\"id\":\"\",\"title\":\"Bad\"}]}\\n'; sleep 2"
                )
                .into(),
            ],
            json!({}),
        )
        .unwrap();
        let event = provider
            .events
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        assert!(matches!(event, ProviderEvent::Error(error)
                if error.contains("provider items") && error.contains("source diagnostic")));
    }

    #[test]
    fn naturally_exited_provider_closes_descendant_output_promptly() {
        let provider = Provider::start(
            &[
                "sh".into(),
                "-c".into(),
                concat!(
                    "sleep 30 & ",
                    "printf '{\"items\":[{\"id\":\"one\",\"title\":\"One\"}]}\\n'"
                )
                .into(),
            ],
            json!({}),
        )
        .unwrap();
        let events = provider.events.as_ref().unwrap();

        assert!(matches!(
            events.recv_timeout(Duration::from_secs(1)).unwrap(),
            ProviderEvent::Snapshot(_)
        ));
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(1)).unwrap(),
            ProviderEvent::Exited { status, .. } if status.success()
        ));
    }

    #[test]
    fn command_failures_include_stderr() {
        let argv = vec![
            "sh".into(),
            "-c".into(),
            "cat >/dev/null; echo nope >&2; exit 7".into(),
        ];
        assert_eq!(
            run(&argv, &json!({})).unwrap_err().to_string(),
            "sh -c cat >/dev/null; echo nope >&2; exit 7 failed: nope"
        );
    }

    #[test]
    fn command_output_capture_is_bounded() {
        let script = "cat >/dev/null; yes x | head -c 131072; yes e | head -c 131072 >&2; exit 7";
        let error = run(&["sh".into(), "-c".into(), script.into()], &json!({})).unwrap_err();

        assert!(error.to_string().len() <= MAX_STDERR_BYTES + script.len() + 32);
    }

    #[test]
    fn command_does_not_wait_for_descendant_pipes() {
        let started = std::time::Instant::now();

        run(
            &["sh".into(), "-c".into(), "sleep 2 & exit 0".into()],
            &json!({}),
        )
        .unwrap();

        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn dropping_provider_terminates_its_process_group() {
        let path = std::env::temp_dir().join(format!(
            "herdr-picker-descendant-{}.pid",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let provider = Provider::start(
            &[
                "sh".into(),
                "-c".into(),
                "sleep 30 & echo $! > \"$1\"; wait".into(),
                "sh".into(),
                path.display().to_string(),
            ],
            json!({}),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let pid = loop {
            if let Ok(contents) = std::fs::read_to_string(&path)
                && let Ok(pid) = contents.trim().parse::<libc::pid_t>()
            {
                break pid;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        };

        drop(provider);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while process_exists(pid) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let alive = process_exists(pid);
        if alive {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_file(path);
        assert!(!alive);
    }

    fn process_exists(pid: libc::pid_t) -> bool {
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}
