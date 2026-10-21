//! `cargo xtask bench-report` run as a program: the `--stored-base` and
//! `--stored-commit` flags reach the comment.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code may unwrap (docs/code-quality.md, \"Lints\"); a failure is the test's"
)]

use std::io::{BufRead as _, BufReader};
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{Deadline, TempDir, finished, group_empties, kill_group};

/// How long a test waits for the child, in real time; a passing run never
/// waits on it, it only bounds a hang.
const CHILD_WITHIN: Duration = Duration::from_secs(30);

/// A two-row budget table no benchmark owns, so the run fails on the table
/// mismatch and the tests read the comment file.
const DOC: &str = "# Performance\n\n## Budgets\n\n\
     | Budget | Ceiling | Gated on | Basis |\n\
     |---|---|---|---|\n\
     | First thing | 5 ms | Linux x86_64 | picked |\n\
     | Second thing | 6 ms | Linux x86_64 | picked |\n";

/// A result file that parses: no metrics, no failures.
const HEAD: &str = r#"{"schema":1,"runs":5,"idle_secs":10,"metrics":{},"failures":[]}"#;

/// A stored file carrying a CPU model.
const STORED: &str =
    r#"{"schema":1,"runs":5,"idle_secs":10,"cpu":"CPU-S","metrics":{},"failures":[]}"#;

/// A directory holding the doc, the head file and the stored file.
struct Files {
    dir: TempDir,
}

impl Files {
    fn new() -> Self {
        let dir = TempDir::new("xtask-bench-report");
        dir.write("doc.md", DOC);
        dir.write("head.json", HEAD);
        dir.write("s.json", STORED);
        Self { dir }
    }

    /// Runs `xtask bench-report` with `args` and returns the comment file.
    /// The table mismatch fails the run; the comment is still written.
    fn comment(&self, args: &[&str]) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .args(["bench-report", "--doc", "doc.md", "--head", "head.json"])
            .args(args)
            .args(["--event", "pull_request", "--comment", "comment.md"])
            .current_dir(self.dir.path());
        isolated(&mut command);
        let child = command.spawn().unwrap();
        let output = finished(
            "xtask bench-report",
            child,
            &[],
            Deadline::after(CHILD_WITHIN),
        );
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        std::fs::read_to_string(self.dir.path().join("comment.md")).unwrap()
    }
}

/// The spawn isolation every test in this file relies on: null stdin,
/// fresh pipes for stdout and stderr, and the child's own process group.
/// A child spawned this way never holds the test's descriptors, so no
/// descendant can hold the captured-output pipe nextest waits on after
/// the test process exits; and the kill in `finished` reaches only the
/// child's group, never the test's.
fn isolated(command: &mut Command) -> &mut Command {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
}

/// Kills the probe's group and reaps it on early return, so a failed
/// assertion leaves nothing running. Disarmed once `finished` reaps it.
struct ProbeGuard {
    group: u32,
    child: Option<Child>,
}

impl ProbeGuard {
    fn new(child: Child) -> Self {
        Self {
            group: child.id(),
            child: Some(child),
        }
    }

    fn disarm(&mut self) -> Child {
        self.child.take().unwrap()
    }
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            match kill_group(self.group, "KILL") {
                Ok(_) | Err(_) => {}
            }
            match child.wait() {
                Ok(_) | Err(_) => {}
            }
        }
    }
}

#[test]
fn the_stored_base_flags_reach_the_comment() {
    let files = Files::new();
    let comment = files.comment(&["--stored-base", "s.json", "--stored-commit", "def5678"]);
    assert!(
        comment.contains(
            "Memory and exact rows compare against the backstop's stored results for def5678."
        ),
        "{comment}"
    );
    assert!(
        comment.contains("- Stored results CPU (def5678): CPU-S"),
        "{comment}"
    );
}

#[test]
fn without_stored_base_the_comment_says_none_was_in_reach() {
    let files = Files::new();
    let comment = files.comment(&[]);
    assert!(
        comment.contains(
            "No stored results for the base or its 9 nearest first-parent ancestors: \
             memory and exact rows are judged against the table alone."
        ),
        "{comment}"
    );
}

#[test]
fn an_unreadable_stored_base_is_reported_not_a_usage_error() {
    let files = Files::new();
    let comment = files.comment(&[
        "--stored-base",
        "missing.json",
        "--stored-commit",
        "def5678",
    ]);
    assert!(
        comment.contains("The stored results for def5678 could not be read ("),
        "{comment}"
    );
}

/// See #2270: nextest once flagged
/// `without_stored_base_the_comment_says_none_was_in_reach` as LEAK with the
/// test passing. The handle is nextest's captured stdout/stderr pipe for the
/// test process: no descendant of this file's spawns can hold it, because
/// every spawn keeps null or piped stdio and never inherits the test's
/// descriptors. What nextest measured instead was its own post-exit grace
/// firing under load: the flagged run took 0.226 s against 0.024 s and
/// 0.025 s for its two siblings, which run the same helper.
#[test]
fn the_spawned_child_holds_no_test_stdio() {
    // The probe prints one ready line, then blocks reading a stdin pipe
    // the test holds open; dropping the pipe releases it. Held on the
    // pipe, never on a real-clock sleep, so the interleaving holds every
    // run. Piped stdin overrides the shared helper's null stdin only for
    // the probe; the real `xtask` spawn keeps null stdin.
    let mut probe = Command::new("sh");
    probe.args(["-c", "echo ready; read line"]);
    isolated(&mut probe);
    probe.stdin(Stdio::piped());
    let mut guard = ProbeGuard::new(probe.spawn().unwrap());
    let group = guard.group;
    // Piped stdin is a fresh pipe the test holds open: the child never
    // inherits the test's stdin.
    let stdin = guard
        .child
        .as_mut()
        .unwrap()
        .stdin
        .take()
        .expect("the probe child has no piped stdin");
    // Piped stdout and stderr are fresh pipes owned by the test: the
    // child never inherits the test's stdout or stderr, the pipe nextest
    // waits on after the test process exits.
    assert!(
        guard.child.as_mut().unwrap().stdout.is_some(),
        "the probe child has no piped stdout"
    );
    assert!(
        guard.child.as_mut().unwrap().stderr.is_some(),
        "the probe child has no piped stderr"
    );
    // Readiness is the ready line, read on a thread through a deadline.
    let out = guard.child.as_mut().unwrap().stdout.take().unwrap();
    let (ready, line) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        let read = BufReader::new(out).read_line(&mut text);
        match ready.send(read.map(|_| text)) {
            Ok(()) | Err(_) => {}
        }
    });
    let line = match Deadline::after(CHILD_WITHIN).recv(&line) {
        Ok(read) => read.expect("the probe's ready line did not read"),
        Err(_) => panic!("waited {CHILD_WITHIN:?} for the probe to print its ready line"),
    };
    assert_eq!(line.trim(), "ready", "the probe printed no ready line");
    // Its own process group, read through `ps` bounded and reaped by
    // `finished` like every other child here.
    let mut lookup = Command::new("ps");
    lookup.args(["-o", "pgid=", "-p", &group.to_string()]);
    isolated(&mut lookup);
    let lookup = lookup.spawn().unwrap();
    let output = finished("ps pgid", lookup, &[], Deadline::after(CHILD_WITHIN));
    assert!(output.status.success(), "{output:?}");
    let pgid: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("ps named no group for {group}: {output:?}"));
    assert_eq!(
        pgid, group,
        "the probe child shares the test's process group"
    );
    // Release is dropping the held stdin: EOF ends the read. `finished`
    // reaps the probe and asserts the group emptied; assert again after
    // it returns, so the test itself states that no process is left
    // behind to hold any handle nextest waits on.
    drop(stdin);
    finished(
        "probe shell",
        guard.disarm(),
        &[],
        Deadline::after(CHILD_WITHIN),
    );
    assert!(
        group_empties(group, fakes::Deadline::after(CHILD_WITHIN)),
        "the probe group {group} is still alive after finished"
    );
}
