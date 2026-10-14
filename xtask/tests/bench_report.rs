//! `cargo xtask bench-report` run as a program: the `--stored-base` and
//! `--stored-commit` flags reach the comment.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code may unwrap (docs/code-quality.md, \"Lints\"); a failure is the test's"
)]

use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::time::Duration;

use fakes::{TempDir, finished, group_empties, kill_group};

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
        let output = finished("xtask bench-report", child, &[], CHILD_WITHIN);
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
/// `without_stored_base_the_comment_says_none_was_in_reach` as LEAK, with
/// the test passing and no process left behind.
///
/// The handle nextest waited on is its captured stdout/stderr pipe for the
/// test process: after the test process exits, nextest waits a short grace
/// for that pipe to close, and flags LEAK while it is still open, which
/// takes a descendant holding the test's stdout or stderr. This test
/// proves no descendant of this file's spawns can hold it:
///
/// - the probe below is spawned through the same `isolated` the real
///   `xtask` child uses, and is held alive on `sleep`'s own clock while
///   the test inspects it mid-run, so the interleaving holds every run;
///   null stdin leaves no stdin handle, and piped stdout/stderr are fresh
///   pipes owned by the test, never the test's descriptors;
/// - the probe leads its own process group (`ps -o pgid=` names its own
///   pid), so the SIGKILL in `finished` reaches only the child;
/// - `bench-report` without `--stored-base` starts no process (no
///   `Command` in `xtask/src/bench.rs` or `xtask/src/bench/`), and
///   `TempDir::drop` only removes files, so the `xtask` child itself is
///   the only process, and it inherits the same isolation;
/// - the test kills the probe's group, `finished` reaps it and asserts
///   the group emptied, and this test asserts `group_empties` again after
///   it returns: a passing run leaves no process behind to hold any
///   handle. The every-path reap of the helper itself is pinned in
///   `crates/fakes/src/finished_tests.rs`.
///
/// What nextest measured instead was its own post-exit grace firing under
/// load: the flagged run took 0.226 s against 0.024 s and 0.025 s for its
/// two siblings, which run the same helper. The pipe had not closed within
/// the grace when the loaded machine scheduled the check; nothing the test
/// starts can hold it open.
#[test]
fn the_spawned_child_holds_no_test_stdio() {
    let mut probe = Command::new("sleep");
    probe.arg("30");
    isolated(&mut probe);
    let child = probe.spawn().unwrap();
    // Null stdin creates no handle: the child reads /dev/null, never the
    // test's stdin.
    assert!(child.stdin.is_none(), "the probe child has a stdin handle");
    // Piped stdout and stderr are fresh pipes owned by the test: the child
    // never inherits the test's stdout or stderr, the pipe nextest waits
    // on after the test process exits.
    assert!(
        child.stdout.is_some(),
        "the probe child has no piped stdout"
    );
    assert!(
        child.stderr.is_some(),
        "the probe child has no piped stderr"
    );
    let pid = child.id();
    // Its own process group, read through `ps` bounded and reaped by
    // `finished` like every other child here.
    let mut lookup = Command::new("ps");
    lookup.args(["-o", "pgid=", "-p", &pid.to_string()]);
    isolated(&mut lookup);
    let lookup = lookup.spawn().unwrap();
    let output = finished("ps pgid", lookup, &[], CHILD_WITHIN);
    assert!(output.status.success(), "{output:?}");
    let pgid: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("ps named no group for {pid}: {output:?}"));
    assert_eq!(pgid, pid, "the probe child shares the test's process group");
    // The probe is still held alive on `sleep`'s clock: kill its group,
    // then `finished` reaps it and asserts the group emptied, and assert
    // again after it returns, so the test itself states that no process is
    // left behind to hold any handle nextest waits on. (`sleep` dies to
    // the SIGKILL, so its status is never success and is not read.)
    assert!(
        kill_group(pid, "KILL").unwrap(),
        "the probe group {pid} was already gone before its kill"
    );
    finished("probe sleep", child, &[], CHILD_WITHIN);
    assert!(
        group_empties(pid, CHILD_WITHIN),
        "the probe group {pid} is still alive after finished"
    );
}
