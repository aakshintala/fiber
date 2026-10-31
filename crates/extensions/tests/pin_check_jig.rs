//! The `pin_check` jig (`docs/testing.md`, "Jigs"): the built `pin_check`
//! binary times declaring and hashing a synthetic repository and prints
//! one summary line.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers; a failure is the test's"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;

/// How long the pin_check jig exchange may take once the binary has started.
/// This file's exchange bound, the start bound (`fakes::START_BOUND`) and the
/// stand-down sum under nextest's 120 s limit (`docs/testing.md`, "Waits and
/// timeouts").
const JIG_DEADLINE: Duration = Duration::from_secs(50);

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/pin_check`), so the test starts no cargo and
/// waits on no build lock. Warming before returning means every exec site
/// holds only a warmed path.
fn example_path() -> fakes::WarmedJig {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    fakes::WarmedJig::warm(path.join("examples").join("pin_check"), &["x"], 2)
}

#[track_caller]
fn pin_check(files: &str, bytes: &str) -> Output {
    let warmed = example_path();
    let child = Command::new(warmed.path())
        .args([files, bytes])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let watchdog = fakes::Watchdog::group(child.id());
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = Deadline::after(JIG_DEADLINE)
        .recv(&finished)
        .expect("the pin_check jig finished")
        .unwrap();
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    output
}

#[test]
fn it_times_a_small_repository_and_prints_one_summary_line() {
    let out = pin_check("2", "64");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    let line = String::from_utf8(out.stdout).unwrap();
    assert!(
        line.starts_with("2 files of 64 bytes (128 bytes) on "),
        "{line}"
    );
    assert!(line.contains("cold "), "{line}");
    assert!(line.contains("warm "), "{line}");
    assert!(line.contains("fastest"), "{line}");
    assert!(line.contains("slowest of 20"), "{line}");
    assert_eq!(line.lines().count(), 1, "{line}");
}
