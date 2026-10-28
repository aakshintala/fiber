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

/// How long the child may run before the test kills it and fails: it fires
/// before the parent's wait, so the two no longer contradict.
const CHILD_DEADLINE: Duration = Duration::from_secs(20);

/// A parent's wait for a re-run child: inside half of nextest's per-test
/// timeout (`.config/nextest.toml`, `docs/testing.md`, "Waits and timeouts").
const RERUN_WITHIN: Duration = Duration::from_secs(30);

/// Re-run bounds keep their margins: the child's budget is below the
/// parent's wait, and the wait stays inside half of nextest's per-test
/// timeout.
const _: () =
    assert!(CHILD_DEADLINE.as_secs() < RERUN_WITHIN.as_secs() && RERUN_WITHIN.as_secs() <= 30);

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
    let out = fakes::rerun_within(
        &this("a_failure_after_a_recorded_signal_reports_only_the_signal"),
        &[(CHILD, "signal")],
        Deadline::after(RERUN_WITHIN),
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
    let out = fakes::rerun_within(
        &this("a_failure_with_no_signal_is_reported"),
        &[(CHILD, "1")],
        Deadline::after(RERUN_WITHIN),
    );
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("fiber_exited"), "{stdout}");
    assert!(stdout.contains("io_failed"), "{stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1);
    assert!(stderr.contains("fiber: x"), "{stderr}");
}

#[test]
fn effective_answerable_keeps_the_base_only_when_not_supervised() {
    use super::effective_answerable;
    // A fresh or resumed session a person may answer stays answerable.
    assert!(effective_answerable(true, false));
    // A supervised session never answers, new or resumed, even when the
    // base run would: `fiber ask` stays unanswerable either way.
    assert!(!effective_answerable(true, true));
    assert!(!effective_answerable(false, false));
    assert!(!effective_answerable(false, true));
}

#[test]
fn an_isolated_fork_adopts_before_reading_its_point() {
    if let Ok(workspace) = std::env::var(CHILD) {
        let args = crate::cli::SessionArgs {
            id: "s_0123456789abcdef".into(),
            workspace: workspace.into(),
            model: None,
            overrides: Vec::new(),
            prompt: Some("check it".into()),
            resume: false,
            credential: None,
            worktree: true,
            rewound_from: None,
            continues: None,
            forked_from: Some("s_aaaaaaaaaaaaaaaa".into()),
            forked_seq: Some(41),
            parent: Some("s_aaaaaaaaaaaaaaaa".into()),
            delegate_id: Some("j_bbbbbbbbbbbbbbbb".into()),
            depth: Some(1),
            added: Vec::new(),
            login_shell: false,
            supervised: false,
        };
        std::process::exit(super::run(
            args,
            fakes::clock::FakeClock::new(),
            Err("not needed".into()),
        ));
    }
    let root = fakes::TempDir::new("fiber-fork-adoption");
    let workspace = root.path().join("fork");
    std::fs::create_dir(&workspace).unwrap();
    // Opening the lock must fail before the missing fork point is read.
    std::fs::create_dir(root.path().join("fork.lock")).unwrap();
    let output = fakes::rerun_within(
        &this("an_isolated_fork_adopts_before_reading_its_point"),
        &[(CHILD, workspace.to_str().unwrap())],
        Deadline::after(RERUN_WITHIN),
    );
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("io_failed"), "{stdout}");
    assert!(stdout.contains("fork.lock"), "{stdout}");
    assert!(!stdout.contains("session_not_found"), "{stdout}");
}

/// Installs the signals, records a real SIGTERM to this process when
/// `signalled`, then reports an `io_failed` failure and exits with what
/// `report` returns.
#[track_caller]
fn report_child(signalled: bool) {
    fakes::within("report_child", Deadline::after(CHILD_DEADLINE), move || {
        let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
        let signals = doors::Signals::install(clock, None).unwrap();
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
