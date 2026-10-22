//! The `draw` jig through its built binary: it draws an events file at a
//! given width and prints the screen as text, and reports usage without
//! both arguments.

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

/// A usage error: exit code 2 with the usage on stderr and no screen, so
/// a renamed argument fails here before the release job.
fn assert_usage(out: &Output) {
    assert_eq!(out.status.code(), Some(2), "a usage error exits 2");
    let stderr = String::from_utf8(out.stderr.clone()).unwrap();
    assert!(
        stderr
            .contains("usage: cargo run -p tui --example draw -- <events.jsonl> <width> [height]"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty(), "a usage error prints no screen");
}

#[test]
fn it_reports_usage_without_arguments() {
    let out = jig::run_jig("draw", &[], "the draw jig finished");
    assert_usage(&out);
}

#[test]
fn a_width_that_is_not_a_number_is_a_usage_error() {
    // A width the jig cannot parse is a usage error, not a defaulted draw.
    let arg = jig::events_file().display().to_string();
    let out = jig::run_jig("draw", &[arg.as_str(), "wide"], "the draw jig finished");
    assert_usage(&out);
}

#[test]
fn it_draws_the_events_file_at_the_given_width() {
    let arg = jig::events_file().display().to_string();
    let out = jig::run_jig("draw", &[arg.as_str(), "80"], "the draw jig finished");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.is_empty(),
        "a drawn screen writes no stderr: {stderr}"
    );
    assert!(!out.stdout.is_empty(), "a drawn screen is not empty");
}
