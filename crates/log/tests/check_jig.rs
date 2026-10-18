//! The `check` jig through its built binary: a session the log wrote
//! passes with its one-line summary, and a line that does not parse fails
//! naming the line.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers outside a #[test]; a failure is the test's"
)]

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::{TestDir, delta, empty, id, session_started};
use fakes::Deadline;
use log::Log;

#[path = "common/mod.rs"]
mod common;

#[path = "support/check.rs"]
mod support;

/// The deadline on each wait: the jig checking a session, and the watchdog
/// standing down after it. A stall fails naming the wait (`docs/testing.md`,
/// "Waits and timeouts").
const DEADLINE: Duration = fakes::MUST_SUCCEED_WITHIN;

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/check`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("check")
}

/// Runs the built `check` binary over `dir` and waits for it under one
/// deadline. The child is in its own process group under a watchdog, so a
/// stall fails naming the wait and leaves no process behind.
#[track_caller]
fn run_jig(dir: &Path) -> Output {
    let child = Command::new(example_path())
        .arg(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let watchdog = fakes::Watchdog::group(child.id());
    let (done, finished) = mpsc::channel();
    thread::spawn(move || match done.send(child.wait_with_output()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    let output = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the check jig finished")
        .unwrap();
    watchdog.stand_down(Duration::from_secs(5));
    output
}

/// A session directory the log wrote, of three durable lines.
fn session(name: &str) -> (TestDir, PathBuf) {
    let tmp = TestDir::new(name);
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    log.append(&session_started(), None, None).unwrap();
    log.append(&delta("x"), None, None).unwrap();
    log.append(&empty("step_started"), None, None).unwrap();
    log.append(&empty("assistant_message_started"), None, None)
        .unwrap();
    let dir = tmp.session(&id("s_1"));
    (tmp, dir)
}

#[test]
fn a_session_the_log_wrote_passes_with_its_summary() {
    let (_tmp, dir) = session("check-jig-ok");
    let out = run_jig(&dir);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "ok: 3 lines, seq contiguous from 0\n"
    );
}

#[test]
fn a_line_that_does_not_parse_fails_naming_the_line() {
    let (_tmp, dir) = session("check-jig-json");
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap()
        .write_all(b"not json\n")
        .unwrap();
    let out = run_jig(&dir);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("line 4"), "{stderr}");
}
