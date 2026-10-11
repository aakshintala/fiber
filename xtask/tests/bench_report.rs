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

use fakes::{TempDir, finished};

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
            .current_dir(self.dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command.spawn().unwrap();
        let output = finished("xtask bench-report", child, &[], CHILD_WITHIN);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        std::fs::read_to_string(self.dir.path().join("comment.md")).unwrap()
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
