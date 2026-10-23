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
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;

/// How long the pin_check jig exchange may take once the binary has started.
/// `.config/nextest.toml` kills a slow test after a 30s period times 4 (120s
/// total); the test's start bound and its exchange bound each count alone
/// against that limit, and a pass-then-hang path adds the start bound, the
/// exchange deadline and the stand-down, a sum that stays under that limit:
/// `docs/testing.md`, "Waits and timeouts", needs nextest's timeout to be at
/// least twice the test's longest single deadline plus its normal run time, so
/// a hang reports which wait expired.
const JIG_DEADLINE: Duration = Duration::from_secs(50);

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/pin_check`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("pin_check")
}

#[track_caller]
fn pin_check(files: &str, bytes: &str) -> Output {
    fakes::warm_up(&example_path(), &["x"], 2);
    let child = Command::new(example_path())
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
