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

use super::{combine_close_result, report};

#[test]
fn nonzero_session_stdout_error_keeps_existing_exit_and_stderr() {
    for error in [
        std::io::Error::from(std::io::ErrorKind::BrokenPipe),
        std::io::Error::other("full disk"),
    ] {
        let result = combine_close_result(Ok(2), Err(error));
        assert_eq!(
            result.expect("the session's nonzero code takes precedence"),
            2
        );
    }

    assert_eq!(
        combine_close_result(
            Ok(0),
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        )
        .expect("BrokenPipe is quiet for a successful session"),
        0
    );
    let failure = combine_close_result(Ok(0), Err(std::io::Error::other("full disk")))
        .expect_err("another copy failure changes success to failure");
    assert_eq!(doors::exit_code(&failure), 1);
    assert_eq!(failure.message, "stdout could not be written: full disk");
    assert_eq!(combine_close_result(Ok(0), Ok(())).unwrap(), 0);
}
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
fn delegate_admission_startup_classification_keeps_asks_closed_and_delegates_open() {
    use super::StartupAdmission;

    for (one_turn, delegate, expected) in [
        (false, false, StartupAdmission::Serve),
        (false, true, StartupAdmission::Serve),
        (true, false, StartupAdmission::Ask),
        (true, true, StartupAdmission::Delegate),
    ] {
        assert_eq!(
            StartupAdmission::for_run(one_turn, delegate),
            expected,
            "one_turn={one_turn}, delegate={delegate}"
        );
    }
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
        let process = crate::startup::Process {
            clock: fakes::clock::FakeClock::new(),
            exe: Err(crate::failed(ErrorCode::IoFailed, "not needed")),
        };
        std::process::exit(super::run(args, &process));
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

/// The session lock stays held from the door close through the exited
/// row's publication: an archive racing the close is refused, and only
/// after the row lands does the lock free, so the row cannot reset an
/// archive that landed in between.
#[test]
fn close_keeps_the_session_lock_until_the_row_is_published() {
    use super::{close_with_result, pause_before_row_arm};
    use contract::SessionId;
    use doors::Session;

    let home = fakes::TempDir::new("fiber-close-row");
    let sessions = home.path().join("projects/-p/sessions");
    let clock = fakes::clock::FakeClock::new();
    let timed: Arc<dyn contract::clock::Clock> = Arc::clone(&clock) as _;
    let id = SessionId("s_0123456789abcdef".to_owned());
    let log = Arc::new(log::Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    // A turn keeps the directory past the door close: an unprompted
    // session leaves nothing behind, row included.
    log.append(
        &contract::events::Event::TurnStarted(contract::events::TurnStarted { input: Vec::new() }),
        None,
        None,
    )
    .unwrap();
    let dir = sessions.join(&id.0);
    let session = Session::open(
        home.path(),
        &dir,
        &log,
        Arc::clone(&timed),
        Vec::new(),
        Box::new(std::io::sink()),
    )
    .unwrap();
    let keeper = crate::keeper::Host::idle(Arc::clone(&timed));
    let (paused_tx, paused_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    pause_before_row_arm(move || {
        paused_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    let workspace = std::path::PathBuf::from("/w");
    let closer = std::thread::spawn({
        let home = home.path().to_path_buf();
        let dir = dir.clone();
        move || {
            close_with_result(
                session,
                log,
                &home,
                &dir,
                &workspace,
                &*clock,
                &keeper,
                Ok(0),
            )
            .expect("closing the session succeeds");
            home
        }
    });
    fakes::Deadline::after(Duration::from_secs(10))
        .recv(&paused_rx)
        .expect("the close pauses before the row");
    // The door is closed but the row is not published: the lock is
    // still held, so an archive now would be refused as held.
    let probe = || {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("session.lock"))
            .unwrap()
            .try_lock()
            .is_err()
    };
    assert!(probe(), "the lock is held through the pause");
    release_tx.send(()).unwrap();
    let home = closer.join().expect("the close returns");
    // The row is published exactly once, and the lock is free after it.
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(home.join("recent.jsonl"))
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["session_id"], "s_0123456789abcdef");
    assert!(!probe(), "the lock frees after the row");
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
