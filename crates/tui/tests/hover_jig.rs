//! The `hover` jig through its built binary: it sends motion reports
//! through the terminal's screen over an events file and prints the
//! frames, bytes and time per report, one line per case.

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

/// Reports per case in this test: the jig defaults to 20,000, which takes
/// minutes in debug, so the test passes an explicit shorter count.
const REPORTS: &str = "50";

/// The checked-in events file both jig tests draw, so the path never
/// changes with a checkout.
fn events_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/hover.jsonl")
}

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/hover`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("hover")
}

/// Runs the built `hover` binary with `args` and waits for it. The child
/// is in its own process group under a watchdog, so a stall fails naming
/// the wait and leaves no process behind.
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
    let output = fakes::wait_output(child, "the hover jig finished");
    watchdog.stand_down(fakes::Deadline::after(Duration::from_secs(5)));
    output
}

/// A usage error: exit code 2 with the usage on stderr and no report, so
/// a renamed argument fails here before the release job.
fn assert_usage(out: &Output) {
    assert_eq!(out.status.code(), Some(2), "a usage error exits 2");
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr.clone()).unwrap();
    assert!(
        stderr.contains(
            "usage: cargo run --release -p tui --example hover -- <events.jsonl> [width] [height] [reports]"
        ),
        "{stderr}"
    );
    assert!(out.stdout.is_empty(), "a usage error prints no report");
}

#[test]
fn reports_that_are_not_a_number_are_a_usage_error() {
    // A report count the jig cannot parse is a usage error, not a
    // defaulted measurement.
    let arg = events_file().display().to_string();
    let out = run_jig(&[arg.as_str(), "160", "48", "many"]);
    assert_usage(&out);
}

#[test]
fn it_measures_the_four_cases_at_the_given_report_count() {
    let arg = events_file().display().to_string();
    let out = run_jig(&[arg.as_str(), "160", "48", REPORTS]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert_ne!(out.status.code(), Some(2));
    assert!(
        stderr.is_empty(),
        "a measurement writes no stderr: {stderr}"
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 6, "{stdout}");
    assert_ne!(lines.len(), 2);
    assert_eq!(lines[0], "160x48, 5 runs per case, median");
    assert_ne!(lines[0], "160x48, 5 runs per case, mean");
    assert_eq!(
        lines[1],
        "case            reports  frames    bytes  time/report"
    );
    for name in ["still pointer", "one target", "target change", "fast sweep"] {
        let line = lines[2..]
            .iter()
            .find(|line| line.get(..15).unwrap_or_default().trim_end() == name)
            .unwrap_or_else(|| panic!("no line for case {name}: {stdout}"));
        // The time and bytes columns are nondeterministic, so only the
        // report count is pinned: the passed count, not the jig default.
        let fields: Vec<&str> = line
            .get(15..)
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        assert_eq!(fields.len(), 5, "{line}");
        assert_eq!(fields[0], REPORTS, "{line}");
        assert_ne!(
            fields[0], "20000",
            "the jig ran the passed count, not its default: {line}"
        );
    }
}
