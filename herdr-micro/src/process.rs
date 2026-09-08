use anyhow::{Context, Result, bail};
use std::{
    io::{self, Read},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Default)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn capture_output(mut reader: impl Read) -> io::Result<CapturedOutput> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((COMMAND_OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > COMMAND_OUTPUT_LIMIT;
    bytes.truncate(COMMAND_OUTPUT_LIMIT);
    io::copy(&mut reader, &mut io::sink())?;
    Ok(CapturedOutput { bytes, truncated })
}

fn join_output(
    reader: thread::JoinHandle<io::Result<CapturedOutput>>,
    stream: &str,
    program: &str,
) -> Result<CapturedOutput> {
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("{stream} reader for {program} panicked"))?
        .with_context(|| format!("read {stream} from {program}"))
}

fn terminate_process_group(process_group: i32) -> io::Result<()> {
    if unsafe { libc::kill(-process_group, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

pub(crate) fn run_command_with_timeout(command: &mut Command, timeout: Duration) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run {program}"))?;
    let process_group = match i32::try_from(child.id()) {
        Ok(process_group) => process_group,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("child process ID is too large");
        }
    };
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || capture_output(stderr));
    let started = Instant::now();
    let waited = (|| -> Result<_> {
        loop {
            if let Some(status) = child
                .try_wait()
                .with_context(|| format!("wait for {program}"))?
            {
                break Ok((status, false));
            }
            if started.elapsed() >= timeout {
                terminate_process_group(process_group)
                    .with_context(|| format!("terminate timed out {program}"))?;
                let status = child
                    .wait()
                    .with_context(|| format!("reap timed out {program}"))?;
                break Ok((status, true));
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if waited.is_err() {
        let _ = terminate_process_group(process_group);
        let _ = child.kill();
        let _ = child.wait();
    }
    let (status, timed_out) = waited?;
    terminate_process_group(process_group)
        .with_context(|| format!("terminate descendants of {program}"))?;
    let captured_stderr = join_output(stderr_reader, "stderr", &program)?;
    let detail = String::from_utf8_lossy(&captured_stderr.bytes)
        .trim()
        .to_owned();
    let truncated = if captured_stderr.truncated {
        " (stderr truncated)"
    } else {
        ""
    };
    if timed_out {
        bail!("{program} timed out after {}s", timeout.as_secs_f64());
    }
    if !status.success() {
        bail!("{program} failed with {status}: {detail}{truncated}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_runner_reports_stderr_and_reaps_timeouts() {
        let error = run_command_with_timeout(
            Command::new("/bin/sh").args(["-c", "printf failure >&2; exit 7"]),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("failure"));

        let started = Instant::now();
        let error = run_command_with_timeout(
            Command::new("/bin/sh").args(["-c", "exec /bin/sleep 5"]),
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));

        let started = Instant::now();
        let error = run_command_with_timeout(
            Command::new("/bin/sh").args(["-c", "while :; do printf x; done"]),
            Duration::from_millis(20),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));

        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("descendant");
        run_command_with_timeout(
            Command::new("/bin/sh")
                .args(["-c", "(sleep 0.2; : > \"$HERDR_TEST_MARKER\") &"])
                .env("HERDR_TEST_MARKER", &marker),
            Duration::from_secs(1),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists(), "background descendant survived");

        let noisy = format!(
            "/usr/bin/yes x | /usr/bin/head -c {} >&2; exit 7",
            COMMAND_OUTPUT_LIMIT * 2
        );
        let error = run_command_with_timeout(
            Command::new("/bin/sh").args(["-c", &noisy]),
            Duration::from_secs(1),
        )
        .unwrap_err();
        let detail = error.to_string();
        assert!(detail.ends_with(" (stderr truncated)"));
        assert!(detail.len() < COMMAND_OUTPUT_LIMIT + 100);
    }
}
