//! Tests for reporting a startup failure: a recorded signal's code wins
//! and prints nothing, and without one the failure is printed
//! (`docs/invocation.md`, "Shutdown").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::ErrorCode;

use super::report;
use fakes::Deadline;

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(10);

/// The child's marker: set, the test installs the signals and reports a
/// failure, with a real SIGTERM sent to its own process or with none.
const CHILD: &str = "FIBER_MAIN_REPORT_CHILD";

/// This test's full path, for `fakes::rerun`: the crate name
/// `module_path!` carries is not part of a test's path.
fn this(test: &str) -> String {
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{module}::{test}")
}

#[test]
fn a_failure_after_a_recorded_signal_reports_only_the_signal() {
    if std::env::var(CHILD).is_ok() {
        report_child(true);
        return;
    }
    let out = fakes::rerun(
        &this("a_failure_after_a_recorded_signal_reports_only_the_signal"),
        &[(CHILD, "signal")],
    );
    assert_eq!(out.status.code(), Some(143), "the signal's code wins");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("fiber_exited"),
        "a recorded signal writes nothing"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("fiber:"),
        "a recorded signal writes nothing"
    );
}

#[test]
fn a_failure_with_no_signal_is_reported() {
    if std::env::var(CHILD).is_ok() {
        report_child(false);
        return;
    }
    let out = fakes::rerun(
        &this("a_failure_with_no_signal_is_reported"),
        &[(CHILD, "1")],
    );
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("fiber_exited"), "{stdout}");
    assert!(stdout.contains("io_failed"), "{stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1);
    assert!(stderr.contains("fiber: x"), "{stderr}");
}

/// Installs the signals, records a real SIGTERM to this process when
/// `signalled`, then reports an `io_failed` failure and exits with what
/// `report` returns.
#[track_caller]
fn report_child(signalled: bool) {
    fakes::within("report_child", CHILD_DEADLINE, move || {
        let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
        let signals = doors::Signals::install(clock).unwrap();
        let (tx, rx) = mpsc::channel();
        signals.arm(Box::new(move || tx.send(()).unwrap()), Box::new(|| {}));
        if signalled {
            fakes::kill_pid(std::process::id(), "TERM").unwrap();
            Deadline::after(CHILD_DEADLINE)
                .recv(&rx)
                .expect("the signal was recorded before reporting");
        }
        let code = report(&signals, crate::failed(ErrorCode::IoFailed, "x"));
        std::process::exit(code);
    });
}
