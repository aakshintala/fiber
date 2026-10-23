//! The `hover` jig through its built binary: it sends motion reports
//! through the terminal's screen over an events file and prints the
//! frames, bytes and time per report, one line per case.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers outside a #[test]; a failure is the test's"
)]

// Shared jig support: the events file, the built binary path and the
// child-process watchdog and wait (see `support/jig.rs`).
#[path = "support/jig.rs"]
mod jig;

use std::process::Output;

/// Reports per case in this test: the jig defaults to 20,000, which takes
/// minutes in debug, so the test passes an explicit shorter count.
const REPORTS: &str = "50";

/// A usage error: exit code 2 with the usage on stderr and no report, so
/// a renamed argument fails here before the release job.
fn assert_usage(out: &Output) {
    assert_eq!(out.status.code(), Some(2), "a usage error exits 2");
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
    let arg = jig::events_file().display().to_string();
    let out = jig::run_jig(
        "hover",
        &[arg.as_str(), "160", "48", "many"],
        "the hover jig finished",
    );
    assert_usage(&out);
}

#[test]
fn zero_reports_are_a_usage_error() {
    // Zero reports would divide by zero when printing the time per
    // report, so the jig rejects it before measuring.
    let arg = jig::events_file().display().to_string();
    let out = jig::run_jig(
        "hover",
        &[arg.as_str(), "160", "48", "0"],
        "the hover jig finished",
    );
    assert_usage(&out);
}

#[test]
fn it_measures_the_four_cases_at_the_given_report_count() {
    let arg = jig::events_file().display().to_string();
    let out = jig::run_jig(
        "hover",
        &[arg.as_str(), "160", "48", REPORTS],
        "the hover jig finished",
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.is_empty(),
        "a measurement writes no stderr: {stderr}"
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 6, "{stdout}");
    assert_eq!(lines[0], "160x48, 5 runs per case, median");
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
    }
}
