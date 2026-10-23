//! The `record` jig through its built binary, on its two paths that
//! send nothing: no arguments prints usage, and a bare header is rejected
//! before any request is built. No live call runs, so no probe key is read
//! and nothing is spent.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers outside a #[test]; a failure is the test's"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

#[path = "support/record.rs"]
mod support;

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/record`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("record")
}

/// Runs the built `record` binary with `args` and waits for it. The child
/// is in its own process group under a watchdog, so a stall fails naming
/// the wait and leaves no process behind.
#[track_caller]
fn run_jig(args: &[&str]) -> Output {
    fakes::warm_up(&example_path(), &[], 2);
    let child = Command::new(example_path())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let watchdog = fakes::Watchdog::group(child.id());
    let output = fakes::wait_output(child, "the record jig finished");
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    output
}

#[test]
fn it_reports_usage_without_arguments() {
    let out = run_jig(&[]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("usage: cargo run -p provider --example record --"),
        "{stderr}"
    );
}

#[test]
fn a_bare_header_is_rejected_before_any_request() {
    let out = run_jig(&[
        "openai-responses",
        "http://127.0.0.1:1",
        "model",
        "key",
        "request.json",
        "out.sse",
        "no-colon",
    ]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("record: a header is NAME:VALUE"),
        "{stderr}"
    );
}
