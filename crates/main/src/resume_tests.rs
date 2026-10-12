//! Tests for the resume helpers: which recorded worktree a resume enters,
//! whether it is recreated, and that a startup failure ends the isolation
//! before it is reported.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use contract::SessionId;
use contract::events::{Event, SessionStarted, Variables, VariablesSource};
use contract::shapes::Worktree;
use contract::{ErrorCode, clock::Clock};
use log::Log;

use super::{enter_worktree, finish, recorded_worktree, recreatable};
use crate::failed;

/// A session directory whose first line is `session_started` with
/// `worktree`, holding its lock as a live session would.
fn session_with(dir: &Path, worktree: Option<Worktree>) -> Log {
    let sessions = dir.parent().unwrap().to_path_buf();
    let id = SessionId(dir.file_name().unwrap().to_string_lossy().into_owned());
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let log = Log::create(&sessions, id, Arc::clone(&clock)).unwrap();
    log.append(
        &Event::SessionStarted(SessionStarted {
            workspace: "/nowhere".to_owned(),
            variables: Variables {
                path: "/usr/bin:/bin".to_owned(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree,
        }),
        None,
        None,
    )
    .unwrap();
    log
}

#[test]
fn finish_ends_before_it_reports() {
    let order = RefCell::new(Vec::new());
    let code = finish(
        || order.borrow_mut().push("end"),
        Err(failed(ErrorCode::IoFailed, "gone")),
        |failure| {
            order.borrow_mut().push("report");
            assert_eq!(failure.code, ErrorCode::IoFailed);
            3
        },
    );
    assert_eq!(code, 3);
    assert_eq!(*order.borrow(), vec!["end", "report"]);
    // Success ends and returns the code, reporting nothing.
    let order = RefCell::new(Vec::new());
    let code = finish(
        || order.borrow_mut().push("end"),
        Ok(7),
        |_| panic!("success reports nothing"),
    );
    assert_eq!(code, 7);
    assert_eq!(*order.borrow(), vec!["end"]);
}

#[test]
fn recreatable_needs_a_repository_and_a_missing_path() {
    let home = fakes::TempDir::new("main-resume-recreatable");
    let missing = home.path().join("wt");
    assert!(
        recreatable(Some(&Worktree {
            path: missing.to_string_lossy().into_owned(),
            branch: "fiber/x".to_owned(),
            repository: Some("/repo/.git".to_owned()),
        })),
        "a recorded repository with a missing path recreates"
    );
    let present = home.path().join("present");
    std::fs::create_dir(&present).unwrap();
    assert!(
        !recreatable(Some(&Worktree {
            path: present.to_string_lossy().into_owned(),
            branch: "fiber/x".to_owned(),
            repository: Some("/repo/.git".to_owned()),
        })),
        "a present path resumes where it is"
    );
    let dangling = home.path().join("dangling");
    std::os::unix::fs::symlink(home.path().join("no-such-target"), &dangling).unwrap();
    assert!(
        !recreatable(Some(&Worktree {
            path: dangling.to_string_lossy().into_owned(),
            branch: "fiber/x".to_owned(),
            repository: Some("/repo/.git".to_owned()),
        })),
        "a dangling symlink exists"
    );
    assert!(
        !recreatable(Some(&Worktree {
            path: missing.to_string_lossy().into_owned(),
            branch: "fiber/x".to_owned(),
            repository: None,
        })),
        "no recorded repository recreates nothing"
    );
    assert!(!recreatable(None), "no recorded worktree recreates nothing");
}

#[test]
fn recorded_worktree_reads_the_first_line() {
    let home = fakes::TempDir::new("main-resume-recorded");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_0123456789abcdef");
    let worktree = Worktree {
        path: "/wt".to_owned(),
        branch: "fiber/x".to_owned(),
        repository: Some("/repo/.git".to_owned()),
    };
    let _log = session_with(&dir, Some(worktree.clone()));
    assert_eq!(recorded_worktree(&dir), Some(worktree));
    // No recorded worktree, an empty log, an unreadable directory and a
    // first line that is no `session_started` all read as none.
    let bare = sessions.join("s_0123456789abcde0");
    let _log = session_with(&bare, None);
    assert_eq!(recorded_worktree(&bare), None);
    let empty = sessions.join("s_0123456789abcde1");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(empty.join("events.jsonl"), "").unwrap();
    assert_eq!(recorded_worktree(&empty), None);
    // A first line that is no `session_started` reads as none.
    let other = sessions.join("s_0123456789abcde3");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("events.jsonl"),
        r#"{"kind":"nope","session_id":"s_0123456789abcde3","ts":1,"schema_version":1,"payload":{}}"#,
    )
    .unwrap();
    assert_eq!(recorded_worktree(&other), None);
    assert_eq!(
        recorded_worktree(&sessions.join("s_0123456789abcde2")),
        None,
        "an unreadable directory reads as none"
    );
}

#[test]
fn enter_worktree_reports_a_lock_it_cannot_open() {
    let home = fakes::TempDir::new("main-resume-enter");
    let worktree = home.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    // A directory at the lock path: the share's open fails.
    let name = worktree.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::create_dir(worktree.with_file_name(format!("{name}.lock"))).unwrap();
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let signals = doors::Signals::install(Arc::clone(&clock)).unwrap();
    let reads = Arc::new(crate::switch::Reads::default());
    match enter_worktree(
        Some(Worktree {
            path: worktree.to_string_lossy().into_owned(),
            branch: "fiber/x".to_owned(),
            repository: Some(home.path().join("repo").to_string_lossy().into_owned()),
        }),
        home.path(),
        &SessionId("s_0123456789abcdef".into()),
        &clock,
        &signals,
        &reads,
    ) {
        Err(error) => {
            assert_eq!(error.failure.code, ErrorCode::IoFailed);
            assert!(error.isolation.is_none(), "nothing was made");
        }
        Ok(_) => panic!("a lock that cannot open fails"),
    }
}
