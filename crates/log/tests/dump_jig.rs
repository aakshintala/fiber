//! The `dump` jig through its built binary: it prints a session
//! directory's events exactly as `events.jsonl` holds them, and reports
//! usage without a directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers outside a #[test]; a failure is the test's"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::{TestDir, delta, empty, id, session_started};
use log::Log;

#[path = "common/mod.rs"]
mod common;

#[path = "support/dump.rs"]
mod support;

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/dump`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("dump")
}

/// Runs the built `dump` binary with `dir`, or with no argument, and waits
/// for it. The child is in its own process group under a watchdog, so a
/// stall fails naming the wait and leaves no process behind.
#[track_caller]
fn run_jig(dir: Option<&Path>) -> Output {
    let mut command = Command::new(example_path());
    if let Some(dir) = dir {
        command.arg(dir);
    }
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let watchdog = fakes::Watchdog::group(child.id());
    let output = fakes::wait_output(child, "the dump jig finished");
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    output
}

/// A session directory the log wrote, of three lines.
fn session(name: &str) -> (TestDir, PathBuf) {
    let tmp = TestDir::new(name);
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    log.append(&session_started(), None, None).unwrap();
    log.append(&delta("x"), None, None).unwrap();
    log.append(&empty("step_started"), None, None).unwrap();
    let dir = tmp.session(&id("s_1"));
    (tmp, dir)
}

#[test]
fn it_prints_the_session_events_byte_for_byte() {
    let (_tmp, dir) = session("dump-jig");
    let out = run_jig(Some(&dir));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert_eq!(out.stdout, std::fs::read(dir.join("events.jsonl")).unwrap());
}

#[test]
fn it_reports_usage_without_a_directory() {
    let out = run_jig(None);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("usage: cargo run -p log --example dump -- <session directory>"),
        "{stderr}"
    );
}
