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

use super::{enter, enter_worktree, finish, in_launch_project, recorded_worktree, recreatable};
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
            supervised: false,
        }),
        None,
        None,
    )
    .unwrap();
    log
}

#[test]
fn in_launch_project_passes_the_recorded_workspace_once() {
    let home = fakes::TempDir::new("main-resume-in-launch-project");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_0123456789abcdef");
    let _log = session_with(&dir, None);
    // The recorded workspace reaches the predicate exactly once.
    let seen = RefCell::new(Vec::new());
    assert!(in_launch_project(&dir, &|started| {
        seen.borrow_mut().push(started.to_owned());
        true
    }));
    assert_eq!(*seen.borrow(), vec!["/nowhere".to_owned()]);
    // A missing log is refused without calling the predicate.
    let seen = RefCell::new(Vec::new());
    assert!(!in_launch_project(
        &sessions.join("s_0123456789abcde9"),
        &|started: &str| {
            seen.borrow_mut().push(started.to_owned());
            true
        }
    ));
    assert!(seen.borrow().is_empty());
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
    let signals = doors::Signals::install(Arc::clone(&clock), None).unwrap();
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
        false,
    ) {
        Err(error) => {
            assert_eq!(error.failure.code, ErrorCode::IoFailed);
            assert!(error.isolation.is_none(), "nothing was made");
        }
        Ok(_) => panic!("a lock that cannot open fails"),
    }
}

#[test]
fn a_hub_present_recreation_enters_the_worktree() {
    let home = fakes::TempDir::new("main-resume-present-hub");
    let worktree = home.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    let shared = doors::share(&worktree).unwrap();
    match enter(&worktree, shared, true) {
        Ok(super::Held::Shared { .. }) => {}
        Ok(_) | Err(_) => panic!("a hub Present enters the worktree"),
    }
    // Canonical on both sides for macOS `/var` versus `/private/var`.
    assert_eq!(
        std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
        std::fs::canonicalize(&worktree).unwrap()
    );
}

#[test]
fn an_ask_present_recreation_keeps_the_launch_directory() {
    let before = std::env::current_dir().unwrap();
    let home = fakes::TempDir::new("main-resume-present-ask");
    let worktree = home.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    let shared = doors::share(&worktree).unwrap();
    match enter(&worktree, shared, false) {
        Ok(super::Held::Shared { .. }) => {}
        Ok(_) | Err(_) => panic!("an ask Present keeps the launch directory"),
    }
    assert_eq!(std::env::current_dir().unwrap(), before);
}

#[test]
fn a_hub_present_recreation_whose_path_vanished_fails_io_failed() {
    let before = std::env::current_dir().unwrap();
    let home = fakes::TempDir::new("main-resume-present-gone");
    let worktree = home.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    let shared = doors::share(&worktree).unwrap();
    std::fs::remove_dir_all(&worktree).unwrap();
    match enter(&worktree, shared, true) {
        Err(error) => {
            assert_eq!(error.failure.code, ErrorCode::IoFailed);
            assert!(
                error
                    .failure
                    .message
                    .contains(worktree.to_string_lossy().as_ref()),
                "{}",
                error.failure.message
            );
            assert!(error.isolation.is_none(), "nothing was made");
        }
        Ok(_) => panic!("a vanished path fails"),
    }
    assert_eq!(std::env::current_dir().unwrap(), before);
}

#[test]
fn a_present_workspace_at_the_metadata_check_enters_on_the_hub_path() {
    let before = std::env::current_dir().unwrap();
    let home = fakes::TempDir::new("main-resume-hub-present");
    let worktree = home.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let signals = doors::Signals::install(Arc::clone(&clock), None).unwrap();
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
        true,
    ) {
        Ok(super::Held::Shared { .. }) => {}
        Ok(_) | Err(_) => panic!("a present workspace enters on the hub path"),
    }
    // Canonical on both sides for macOS `/var` versus `/private/var`.
    assert_eq!(
        std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
        std::fs::canonicalize(&worktree).unwrap()
    );
    std::env::set_current_dir(&before).unwrap();
}

#[test]
fn a_present_workspace_at_the_metadata_check_keeps_the_launch_directory_for_ask_resume() {
    let before = std::env::current_dir().unwrap();
    let home = fakes::TempDir::new("main-resume-ask-present");
    let worktree = home.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let signals = doors::Signals::install(Arc::clone(&clock), None).unwrap();
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
        false,
    ) {
        Ok(super::Held::Shared { .. }) => {}
        Ok(_) | Err(_) => panic!("a present workspace keeps the launch directory"),
    }
    assert_eq!(std::env::current_dir().unwrap(), before);
}
