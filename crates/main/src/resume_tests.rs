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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::SessionId;
use contract::events::{
    Event, ExtensionStateSet, OnFork, Parent, Rewind, SessionStarted, Variables, VariablesSource,
};
use contract::shapes::{Point, Worktree};
use contract::{ErrorCode, JobId, Seq, clock::Clock};
use log::Log;
use serde_json::{Value, json};

use super::{
    chain_seed, enter, enter_worktree, finish, fork_seed, in_launch_project, recorded_worktree,
    recreatable, resume_refused,
};
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
fn a_recreatable_worktree_is_allowed_outside_the_launch_project() {
    let home = fakes::TempDir::new("main-resume-recreatable-outside");
    // A missing path with a recorded repository recreates, so the launch
    // predicate is never consulted even though it would refuse. The session
    // log is valid, so `in_launch_project` would reach the predicate if the
    // recreatable check did not short-circuit first.
    let recorded = Worktree {
        path: home.path().join("wt").to_string_lossy().into_owned(),
        branch: "fiber/x".to_owned(),
        repository: Some("/repo/.git".to_owned()),
    };
    let dir = home.path().join("sessions").join("s_0123456789abcdef");
    let _log = session_with(&dir, Some(recorded.clone()));
    let seen = RefCell::new(Vec::new());
    assert!(
        !resume_refused(Some(&recorded), &dir, &|started: &str| {
            seen.borrow_mut().push(started.to_owned());
            false
        }),
        "a missing but recreatable worktree is allowed outside the launch project"
    );
    assert!(
        seen.borrow().is_empty(),
        "the launch predicate is not called for a recreatable worktree"
    );
    // The same outside predicate refuses a session with no recorded
    // worktree, so the two cases stay distinguished.
    let dir = home.path().join("sessions").join("s_0123456789abcde0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("events.jsonl"),
        json!({
            "kind": "session_started",
            "session_id": "s_0123456789abcde0",
            "ts": 1,
            "schema_version": 1,
            "payload": {
                "workspace": "/nowhere",
                "variables": {"path": "/usr/bin:/bin", "names": [], "source": "inherited"},
            },
        })
        .to_string()
            + "\n",
    )
    .unwrap();
    let seen = RefCell::new(Vec::new());
    assert!(
        resume_refused(None, &dir, &|started: &str| {
            seen.borrow_mut().push(started.to_owned());
            false
        }),
        "a non-recreatable session outside the launch project is refused"
    );
    assert_eq!(*seen.borrow(), vec!["/nowhere".to_owned()]);
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
            assert!(
                matches!(&error.failure, doors::SessionError::SharedIo { .. }),
                "a lock that cannot open fails: {error:?}"
            );
            assert_eq!(error.failure.code(), ErrorCode::IoFailed);
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
            assert!(
                matches!(
                    &error.failure,
                    doors::SessionError::Forwarded(failure)
                    if failure.code == ErrorCode::IoFailed
                        && failure
                            .message
                            .contains(worktree.to_string_lossy().as_ref())
                ),
                "a vanished path fails entering: {error:?}"
            );
            assert_eq!(error.failure.code(), ErrorCode::IoFailed);
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

/// A parent log holding two `Latest` writes for one key: the first exactly
/// at the fork's point, the second after it. The value at the point differs
/// from the end value, so a fold that reads past the point fails.
fn latest_parent(home: &fakes::TempDir) -> (PathBuf, SessionId, Seq) {
    let sessions = home.path().join("sessions");
    let id = SessionId("s_0123456789abcdef".into());
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let log = Log::create(&sessions, id.clone(), Arc::clone(&clock)).unwrap();
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
            worktree: None,
            supervised: false,
        }),
        None,
        None,
    )
    .unwrap();
    let point = log
        .append(
            &Event::ExtensionStateSet(ExtensionStateSet {
                extension: "fiber.test/goal".into(),
                key: "goal".into(),
                value: json!("at point"),
                on_fork: OnFork::Latest,
            }),
            None,
            None,
        )
        .unwrap()
        .seq
        .unwrap();
    log.append(
        &Event::ExtensionStateSet(ExtensionStateSet {
            extension: "fiber.test/goal".into(),
            key: "goal".into(),
            value: json!("later"),
            on_fork: OnFork::Latest,
        }),
        None,
        None,
    )
    .unwrap();
    (sessions, id, point)
}

/// A child continuing `parent` at `point`: a fork when it carries `parent`,
/// a rewind otherwise, both with a workspace-carrying first line.
fn continuing_child(
    sessions: &Path,
    id: &str,
    parent: &SessionId,
    point: Seq,
    delegate: bool,
) -> SessionId {
    let id = SessionId(id.into());
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let log = Log::create(sessions, id.clone(), Arc::clone(&clock)).unwrap();
    log.append(
        &Event::SessionStarted(SessionStarted {
            workspace: "/nowhere".to_owned(),
            variables: Variables {
                path: "/usr/bin:/bin".to_owned(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: delegate.then(|| Parent {
                session_id: parent.clone(),
                delegate_id: JobId("j_0123456789abcdef".into()),
            }),
            forked_from: Some(Point {
                session_id: parent.clone(),
                seq: point,
            }),
            rewind: (!delegate).then(|| Rewind {
                summary: None,
                note: "cut here".to_owned(),
            }),
            worktree: None,
            supervised: false,
        }),
        None,
        None,
    )
    .unwrap();
    id
}

/// The `goal` value the seed carries: the rule rides beside it, but the
/// value is what the tests pin.
fn seed_goal(seed: &extensions::StateSeed) -> Value {
    seed.get("fiber.test/goal")
        .and_then(|keys| keys.get("goal"))
        .map(|(value, _)| value.clone())
        .unwrap()
}

#[test]
fn fork_boundaries_fold_latest_at_the_point() {
    let home = fakes::TempDir::new("main-resume-fork-latest");
    let (sessions, parent, point) = latest_parent(&home);
    let child = continuing_child(&sessions, "s_1111111111111111", &parent, point, true);
    // The resume path classifies the boundary by the following child: a
    // child carrying `parent` folds `Latest` as of the point, excluding
    // the write after it.
    let seed = chain_seed(&sessions, &log::history(&sessions.join(&child.0)).unwrap()).unwrap();
    assert_eq!(seed_goal(&seed), json!("at point"));
    // The fresh-fork path folds the bounded terminal as a fork too.
    let seed = fork_seed(
        &sessions,
        &log::history_to(&sessions.join(&parent.0), point).unwrap(),
    )
    .unwrap();
    assert_eq!(seed_goal(&seed), json!("at point"));
}

#[test]
fn rewind_boundaries_fold_latest_at_the_end() {
    let home = fakes::TempDir::new("main-resume-rewind-latest");
    let (sessions, parent, point) = latest_parent(&home);
    let child = continuing_child(&sessions, "s_1111111111111111", &parent, point, false);
    // A child without `parent` is a rewind: it keeps today's full-log
    // fold, reading `Latest` at the end.
    let seed = chain_seed(&sessions, &log::history(&sessions.join(&child.0)).unwrap()).unwrap();
    assert_eq!(seed_goal(&seed), json!("later"));
    // The fresh-rewind terminal, a bounded segment with no follower,
    // folds as a rewind too.
    let seed = chain_seed(
        &sessions,
        &log::history_to(&sessions.join(&parent.0), point).unwrap(),
    )
    .unwrap();
    assert_eq!(seed_goal(&seed), json!("later"));
}

#[test]
fn a_following_child_without_a_workspace_fails_the_seed_as_corrupt() {
    let home = fakes::TempDir::new("main-resume-fork-corrupt");
    let (sessions, parent, point) = latest_parent(&home);
    let child = continuing_child(&sessions, "s_1111111111111111", &parent, point, true);
    // Strip the workspace: the history walk still reads the chain, but
    // the first-line read the seed classifies by no longer parses.
    let path = sessions.join(&child.0).join("events.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines = text.lines();
    let mut first: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    first
        .as_object_mut()
        .unwrap()
        .get_mut("payload")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("workspace");
    let mut rewritten = serde_json::to_string(&first).unwrap();
    rewritten.push('\n');
    for line in lines {
        rewritten.push_str(line);
        rewritten.push('\n');
    }
    std::fs::write(&path, rewritten).unwrap();
    let segments = log::history(&sessions.join(&child.0)).unwrap();
    let error = chain_seed(&sessions, &segments).unwrap_err();
    assert!(
        matches!(error, log::Error::Pointer { .. }),
        "a corrupt following-child header fails closed, not as a full-log fold: {error:?}"
    );
}

#[test]
fn a_vanished_following_child_fails_the_seed_as_missing() {
    let home = fakes::TempDir::new("main-resume-fork-missing");
    let (sessions, parent, point) = latest_parent(&home);
    let child = continuing_child(&sessions, "s_1111111111111111", &parent, point, true);
    let child_dir = sessions.join(&child.0);
    let segments = log::history(&child_dir).unwrap();
    std::fs::remove_dir_all(&child_dir).unwrap();
    let error = chain_seed(&sessions, &segments).unwrap_err();
    assert!(
        matches!(error, log::Error::NotFound(_)),
        "a missing following-child header fails as missing: {error:?}"
    );
}
