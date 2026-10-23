//! The process boundary the loop writes (`docs/events.md`, "Process
//! boundary"): `fiber_started`, and `fiber_exited` from the loop's
//! `Summary`. The exit's tests live beside the summary in
//! `crates/loop/src/process_tests.rs`, where `observe` is reachable; only
//! the `fiber_started` tests, which need no `Summary`, stay here.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::path::PathBuf;

use contract::SessionId;
use log::Log;
use r#loop::fiber_started;

/// A session log in a temporary directory, removed on drop.
struct Session {
    _root: fakes::TempDir,
    dir: PathBuf,
    log: Log,
}

impl Session {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-process");
        let log = Log::create(
            root.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        let dir = root.path().join("s_1");
        Self {
            dir,
            _root: root,
            log,
        }
    }
}

#[test]
fn fiber_started_is_the_first_line_and_names_the_version() {
    let session = Session::new();

    fiber_started(&session.log, "1.2.3", false).unwrap();

    let lines = log::read(&session.dir).unwrap();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["fiber_started"]
    );
    assert_eq!(lines[0].payload["version"], "1.2.3");
    assert_eq!(lines[0].payload["resumed"], false);
}

#[test]
fn a_resumed_fiber_started_marks_the_session_resumed() {
    let session = Session::new();

    fiber_started(&session.log, "1.2.3", true).unwrap();

    let lines = log::read(&session.dir).unwrap();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["fiber_started"]
    );
    assert_eq!(lines[0].payload["resumed"], true);
}
