// Subprocess-backed checkout facts: one `git status` per scan, `gh` for PRs.
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const GIT_TIMEOUT: Duration = Duration::from_secs(15);
const GH_TIMEOUT: Duration = Duration::from_secs(5);

/// What one `git status --porcelain=v2 --branch` run tells us.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Checkout {
    /// Current branch; `None` when detached.
    pub branch: Option<String>,
    /// Staged, unstaged, or untracked changes present (submodules included).
    pub dirty: bool,
}

/// `None` when `cwd` is not inside a git repository or git fails. The
/// repository's own `status.*` settings apply (untracked files, submodules).
pub fn scan(cwd: &Path) -> Option<Checkout> {
    let output = run_bounded(
        Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(cwd)
            .args(["status", "--porcelain=v2", "--branch"]),
        GIT_TIMEOUT,
    )?;
    Some(parse_status(&String::from_utf8_lossy(&output)))
}

pub fn parse_status(output: &str) -> Checkout {
    let mut checkout = Checkout::default();
    for line in output.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            checkout.branch = (head != "(detached)").then(|| head.to_owned());
        } else if !line.starts_with('#') && !line.is_empty() {
            checkout.dirty = true;
        }
    }
    checkout
}

/// Directories whose changes can alter `scan(cwd)`: the working tree root and,
/// for linked worktrees, their own git directory (`HEAD`, `index`) outside it.
/// The shared object store and refs are deliberately not watched. `None` when
/// `cwd` is not inside a repository.
pub fn watch_roots(cwd: &Path) -> Option<Vec<PathBuf>> {
    let output = run_bounded(
        Command::new("git").arg("-C").arg(cwd).args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
        ]),
        GIT_TIMEOUT,
    )?;
    let output = String::from_utf8_lossy(&output);
    let mut lines = output.lines().map(PathBuf::from);
    let toplevel = lines.next()?;
    let mut roots = vec![toplevel.clone()];
    roots.extend(
        lines
            .next()
            .filter(|git_dir| !git_dir.starts_with(&toplevel)),
    );
    Some(roots)
}

#[derive(Debug, PartialEq, Eq)]
pub enum PrAnswer {
    Found(String),
    NotFound,
    /// `gh` missing, offline, rate-limited, timed out, or not a GitHub repo.
    Failed,
}

/// The open PR whose head is `branch`.
pub fn pr_number(cwd: &Path, branch: &str) -> PrAnswer {
    let Some(output) = run_bounded(
        Command::new("gh")
            .current_dir(cwd)
            .env("GH_PROMPT_DISABLED", "1")
            .args(["pr", "list", "--head", branch, "--state", "open"])
            .args(["--json", "number", "--jq", ".[0].number"]),
        GH_TIMEOUT,
    ) else {
        return PrAnswer::Failed;
    };
    let number = String::from_utf8_lossy(&output).trim().to_owned();
    if !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()) {
        PrAnswer::Found(number)
    } else {
        PrAnswer::NotFound
    }
}

/// Stdout of a successful run, or `None` on failure or once `timeout` passes
/// without the child both closing stdout and exiting. On timeout the child's
/// whole process group is killed, so helpers it spawned (`gh` runs `git`)
/// cannot outlive it holding the pipe open.
pub fn run_bounded(command: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    // Drain the pipe off-thread so a chatty child cannot block on it.
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.read_to_end(&mut output);
        let _ = sender.send(output);
    });
    let deadline = Instant::now() + timeout;
    let output = receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok();
    // Normally the child has already exited once stdout closes; poll only for
    // the odd one that lingers.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if output.is_some() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => break None,
        }
    };
    let Some(status) = status else {
        // SAFETY: the child is not reaped yet, so its pid still names the
        // process group we created for it.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.wait();
        return None;
    };
    status.success().then_some(output?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn parses_branch_and_dirtiness() {
        let clean = parse_status(
            "# branch.oid 0123abcd\n# branch.head feature\n# branch.upstream origin/feature\n# branch.ab +1 -0\n",
        );
        assert_eq!(
            clean,
            Checkout {
                branch: Some("feature".into()),
                dirty: false
            }
        );
        let untracked = parse_status("# branch.oid (initial)\n# branch.head nascent\n? new-file\n");
        assert_eq!(untracked.branch.as_deref(), Some("nascent"));
        assert!(untracked.dirty);
        let detached = parse_status(
            "# branch.oid 0123abcd\n# branch.head (detached)\n1 .M N... 100644 100644 100644 abc def tracked\n",
        );
        assert_eq!(detached.branch, None);
        assert!(detached.dirty);
    }

    #[test]
    fn scan_tracks_staged_unstaged_untracked_and_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        // Git reports canonical paths.
        let repo = &dir.path().canonicalize().unwrap();
        assert_eq!(scan(repo), None);
        assert_eq!(watch_roots(repo), None);
        git(repo, &["init", "--quiet", "-b", "main"]);
        assert_eq!(
            scan(repo).unwrap(),
            Checkout {
                branch: Some("main".into()),
                dirty: false
            }
        );
        fs::write(repo.join("tracked"), "initial").unwrap();
        assert!(scan(repo).unwrap().dirty);
        git(repo, &["add", "tracked"]);
        assert!(scan(repo).unwrap().dirty);
        git(
            repo,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "initial",
            ],
        );
        assert!(!scan(repo).unwrap().dirty);
        fs::write(repo.join("tracked"), "modified").unwrap();
        assert!(scan(repo).unwrap().dirty);
        git(repo, &["checkout", "--", "tracked"]);
        fs::write(repo.join(".git/info/exclude"), "ignored\nchild/\n").unwrap();
        fs::write(repo.join("ignored"), "ignored").unwrap();
        assert!(!scan(repo).unwrap().dirty);

        // A pane deep inside the tree still watches (and reports) the whole repo.
        let nested = repo.join("src/deep");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("file"), "untracked").unwrap();
        assert_eq!(watch_roots(&nested).unwrap(), vec![repo.clone()]);
        assert!(scan(&nested).unwrap().dirty);
        fs::remove_dir_all(repo.join("src")).unwrap();

        let child = repo.join("child");
        git(
            repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                child.to_str().unwrap(),
            ],
        );
        assert_eq!(watch_roots(repo).unwrap(), vec![repo.clone()]);
        assert_eq!(
            watch_roots(&child).unwrap(),
            vec![child.clone(), repo.join(".git/worktrees/child")]
        );
        let detached = scan(&child).unwrap();
        assert_eq!(detached.branch, None);
        assert!(!detached.dirty);
        fs::write(child.join("tracked"), "changed child").unwrap();
        assert!(scan(&child).unwrap().dirty);
        assert!(!scan(repo).unwrap().dirty);
    }

    #[test]
    fn run_bounded_enforces_deadline_and_exit_status() {
        let started = Instant::now();
        assert_eq!(
            run_bounded(
                Command::new("sh").args(["-c", "exec sleep 10"]),
                Duration::from_millis(50)
            ),
            None
        );
        // Closing stdout early does not escape the deadline either.
        assert_eq!(
            run_bounded(
                Command::new("sh").args(["-c", "exec 1>&-; exec sleep 10"]),
                Duration::from_millis(50)
            ),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            run_bounded(
                Command::new("sh").args(["-c", "exit 1"]),
                Duration::from_secs(1)
            ),
            None
        );
        assert_eq!(
            run_bounded(Command::new("printf").arg("123"), Duration::from_secs(1)),
            Some(b"123".to_vec())
        );
    }
}
