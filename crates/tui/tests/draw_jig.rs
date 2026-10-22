//! The `draw` jig through its built binary: it draws an events file at a
//! given width and prints the screen as text, and reports usage without
//! both arguments.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers outside a #[test]; a failure is the test's"
)]

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// The checked-in events file both jig tests draw, so the path never
/// changes with a checkout.
fn events_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/hover.jsonl")
}

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/draw`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("draw")
}

/// Runs the built `draw` binary with `args` and waits for it. The child is
/// in its own process group under a watchdog, so a stall fails naming the
/// wait and leaves no process behind.
#[track_caller]
fn run_jig(args: &[&str]) -> Output {
    let child = Command::new(example_path())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let watchdog = fakes::Watchdog::group(child.id());
    let output = fakes::wait_output(child, "the draw jig finished");
    watchdog.stand_down(fakes::Deadline::after(Duration::from_secs(5)));
    output
}

/// A usage error: exit code 2 with the usage on stderr and no screen, so
/// a renamed argument fails here before the release job.
fn assert_usage(out: &Output) {
    assert_eq!(out.status.code(), Some(2), "a usage error exits 2");
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr.clone()).unwrap();
    assert!(
        stderr
            .contains("usage: cargo run -p tui --example draw -- <events.jsonl> <width> [height]"),
        "{stderr}"
    );
    assert_ne!(stderr.lines().count(), 0, "a usage error names the usage");
    assert!(out.stdout.is_empty(), "a usage error prints no screen");
}

#[test]
fn it_reports_usage_without_arguments() {
    let out = run_jig(&[]);
    assert_usage(&out);
}

#[test]
fn a_width_that_is_not_a_number_is_a_usage_error() {
    // A width the jig cannot parse is a usage error, not a defaulted draw.
    let arg = events_file().display().to_string();
    let out = run_jig(&[arg.as_str(), "wide"]);
    assert_usage(&out);
}

#[test]
fn it_draws_the_events_file_at_the_given_width() {
    let arg = events_file().display().to_string();
    let out = run_jig(&[arg.as_str(), "80"]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert_ne!(out.status.code(), Some(2));
    assert!(
        stderr.is_empty(),
        "a drawn screen writes no stderr: {stderr}"
    );
    assert!(!out.stdout.is_empty(), "a drawn screen is not empty");
}
