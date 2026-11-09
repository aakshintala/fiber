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
    Event, ExtensionStateSet, FiberStarted, OnFork, Parent, Rewind, SessionStarted, Variables,
    VariablesSource,
};
use contract::shapes::{Point, Worktree};
use contract::{ErrorCode, JobId, Seq, clock::Clock, shapes::Failure};
use log::Log;
use serde_json::{Value, json};

use super::{
    chain_seed, enter, enter_worktree, finish, fork_seed, in_launch_project, recorded_worktree,
    recreatable, resume_refused,
};
use crate::failed;
use crate::session_command::ResumeStart;

#[test]
fn keeper_acquiring_the_lock_after_precheck_is_refused_without_attach() {
    use std::sync::mpsc;
    use std::time::Duration;
    let home = fakes::TempDir::new("main-keeper-resume-race");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_0123456789abcdef");
    drop(session_with(&dir, None));
    let clock = fakes::clock::FakeClock::new();
    let (observed, early) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (send, result) = mpsc::channel();
    let worker_sessions = sessions.clone();
    let worker_clock = clock.clone();
    let worker_home = home.path().to_path_buf();
    std::thread::spawn(move || {
        super::before_resume_open(move || {
            observed.send(()).unwrap();
            fakes::Deadline::after(Duration::from_secs(10))
                .recv(&released)
                .unwrap();
        });
        let mut attached = 0;
        let result = super::open_or_attach(
            &worker_home,
            &worker_sessions,
            SessionId("s_0123456789abcdef".into()),
            worker_clock,
            |_| {
                attached += 1;
                7
            },
        );
        send.send((result.err(), attached)).unwrap();
    });
    fakes::Deadline::after(Duration::from_secs(10))
        .recv(&early)
        .unwrap();
    let (published, held) = mpsc::channel();
    std::thread::spawn(move || {
        published
            .send(
                Log::open_as(
                    &sessions,
                    SessionId("s_0123456789abcdef".into()),
                    clock,
                    "keeper 4242",
                )
                .unwrap(),
            )
            .unwrap();
    });
    let _keeper = fakes::Deadline::after(Duration::from_secs(10))
        .recv(&held)
        .unwrap();
    release.send(()).unwrap();
    let (code, attached) = fakes::Deadline::after(Duration::from_secs(10))
        .recv(&result)
        .unwrap();
    assert_eq!(code, Some(1));
    assert_eq!(
        attached, 0,
        "the Held branch must recheck the live keeper label"
    );
}

#[test]
fn keeper_precheck_allows_pid_attach_and_stale_unlocked_recovery() {
    use std::time::Duration;
    let home = fakes::TempDir::new("main-keeper-resume-controls");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_0123456789abcdef");
    let held = session_with(&dir, None);
    let (send, result) = std::sync::mpsc::channel();
    let worker_sessions = sessions.clone();
    let worker_home = home.path().to_path_buf();
    std::thread::spawn(move || {
        let mut attached = 0;
        let result = super::open_or_attach(
            &worker_home,
            &worker_sessions,
            SessionId("s_0123456789abcdef".into()),
            fakes::clock::FakeClock::new(),
            |_| {
                attached += 1;
                7
            },
        );
        send.send((result.err(), attached)).unwrap();
    });
    let (code, attached) = fakes::Deadline::after(Duration::from_secs(10))
        .recv(&result)
        .unwrap();
    assert_eq!((code, attached), (Some(7), 1));
    drop(held);
    std::fs::write(dir.join("session.lock"), "keeper 4242\n").unwrap();
    let (send, result) = std::sync::mpsc::channel();
    let worker_home = home.path().to_path_buf();
    std::thread::spawn(move || {
        let mut attached = 0;
        let result = super::open_or_attach(
            &worker_home,
            &sessions,
            SessionId("s_0123456789abcdef".into()),
            fakes::clock::FakeClock::new(),
            |_| {
                attached += 1;
                7
            },
        );
        send.send((result.is_ok(), attached)).unwrap();
    });
    assert_eq!(
        fakes::Deadline::after(Duration::from_secs(10))
            .recv(&result)
            .unwrap(),
        (true, 0)
    );
}

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
    // One `fiber_started` per simulated process generation, as the original
    // session and one resume each write (`docs/invocation.md`, "Lifecycle"):
    // the log under test has the envelopes the version reader reads.
    for _ in 0..2 {
        log.append(
            &Event::FiberStarted(FiberStarted {
                version: "test".to_owned(),
                resumed: true,
            }),
            None,
            None,
        )
        .unwrap();
    }
    log
}

/// Rewrites the `occurrence`th `fiber_started` line's (0-based)
/// `schema_version` token from `from` to `to` (e.g. `"1"` to `"x"`) with
/// one equal-length `replacen`, so the file length never moves and the
/// log stays a mixed-version history. A non-JSON token such as a bare
/// `x` fails the line parse, which is exactly the reader's `Err` arm.
fn forge_fiber_started_version(dir: &Path, occurrence: usize, from: &str, to: &str) {
    assert_eq!(from.len(), to.len(), "the forge keeps the file length");
    let current = format!("\"schema_version\":{from}");
    let forged = format!("\"schema_version\":{to}");
    let path = dir.join("events.jsonl");
    let before = std::fs::read_to_string(&path).unwrap();
    assert!(before.ends_with('\n'), "the log ends on a complete line");
    let mut lines: Vec<String> = before.lines().map(str::to_owned).collect();
    let targets: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            serde_json::from_str::<Value>(line).is_ok_and(|line| line["kind"] == "fiber_started")
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(targets.len(), 2, "the log holds two `fiber_started` lines");
    assert!(
        occurrence < targets.len(),
        "the forged occurrence exists: {occurrence} of {}",
        targets.len()
    );
    let at = targets[occurrence];
    let replaced = lines[at].replacen(&current, &forged, 1);
    assert_ne!(lines[at], replaced, "the forged line carries {current}");
    lines[at] = replaced;
    let after = lines.join("\n") + "\n";
    assert_eq!(after.len(), before.len(), "the forge changes no length");
    std::fs::write(&path, after).unwrap();
}

#[test]
fn keeper_schema_version_reports_a_readable_log_as_current() {
    let home = fakes::TempDir::new("main-resume-version-readable");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_0123456789abcdef");
    let _log = session_with(&dir, None);
    assert_eq!(
        super::keeper_schema_version(&dir),
        Some(u64::from(contract::SCHEMA_VERSION)),
        "an untouched log reads at this build's version"
    );
}

#[test]
fn keeper_schema_version_names_the_latest_in_a_mixed_history() {
    let home = fakes::TempDir::new("main-resume-version-mixed");
    let sessions = home.path().join("sessions");
    let dir = sessions.join("s_0123456789abcdef");
    let _log = session_with(&dir, None);
    let next = u64::from(contract::SCHEMA_VERSION) + 1;
    forge_fiber_started_version(
        &dir,
        1,
        &contract::SCHEMA_VERSION.to_string(),
        &next.to_string(),
    );
    assert_eq!(
        super::keeper_schema_version(&dir),
        Some(next),
        "a `[v1, v2]` history declines naming the latest"
    );
}

#[test]
fn keeper_schema_version_refusal_names_the_keepers_version() {
    let home = fakes::TempDir::new("main-resume-version-refusal");
    let sessions = home.path().join("sessions");
    let id = SessionId("s_0123456789abcdef".into());
    let dir = sessions.join(&id.0);
    drop(session_with(&dir, None));
    let next = u64::from(contract::SCHEMA_VERSION) + 1;
    forge_fiber_started_version(
        &dir,
        1,
        &contract::SCHEMA_VERSION.to_string(),
        &next.to_string(),
    );
    // A keeper-held lock at an unreadable version declines instead of
    // attaching: the lock is held through the resume's failed open.
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let _keeper = Log::open_as(&sessions, id.clone(), Arc::clone(&clock), "keeper 4242").unwrap();
    let (result, attached) = bounded_open(home.path().to_path_buf(), sessions, id.clone(), clock);
    assert_eq!(result.err(), Some(1), "a refused resume exits 1");
    assert_eq!(attached, 0, "a keeper-held lock never attaches");
    // The refusal built from that version names the keeper's version.
    let keeper = log::Error::Held {
        session: id.0.clone(),
        holder: "keeper 4242".to_owned(),
    };
    let failure = super::keeper_refusal(&dir, &id, keeper);
    let mismatch = doors::SessionError::VersionMismatch {
        id: id.clone(),
        version: next,
    };
    assert_eq!(failure.code, ErrorCode::SessionHeld);
    assert_eq!(failure.code, mismatch.code());
    assert_eq!(failure.message, mismatch.to_string());
}

#[test]
fn keeper_schema_version_ignores_a_forged_first_line() {
    let home = fakes::TempDir::new("main-resume-version-reverse");
    let sessions = home.path().join("sessions");
    let id = SessionId("s_0123456789abcdef".into());
    let dir = sessions.join(&id.0);
    let _log = session_with(&dir, None);
    forge_fiber_started_version(
        &dir,
        0,
        &contract::SCHEMA_VERSION.to_string(),
        &(u64::from(contract::SCHEMA_VERSION) + 1).to_string(),
    );
    assert_eq!(
        super::keeper_schema_version(&dir),
        Some(u64::from(contract::SCHEMA_VERSION)),
        "a `[v2, v1]` history reports the latest readable version"
    );
    // The refusal stays the bare `Held` and never names the forged first.
    let keeper = log::Error::Held {
        session: id.0.clone(),
        holder: "keeper 4242".to_owned(),
    };
    let failure = super::keeper_refusal(&dir, &id, keeper);
    assert_eq!(failure.code, ErrorCode::SessionHeld);
    assert_eq!(
        failure.message,
        log::Error::Held {
            session: id.0.clone(),
            holder: "keeper 4242".to_owned(),
        }
        .to_string(),
        "a readable latest keeps today's bare refusal"
    );
}

#[test]
fn keeper_schema_version_falls_back_without_a_parseable_version() {
    let home = fakes::TempDir::new("main-resume-version-unparseable");
    let sessions = home.path().join("sessions");
    let id = SessionId("s_0123456789abcdef".into());
    let dir = sessions.join(&id.0);
    let _log = session_with(&dir, None);
    // The earlier `fiber_started` line still parses at this build's
    // version while the latest carries a bare `x`: invalid JSON fails
    // the line parse, which is exactly the reader's `Err` arm, so no
    // keeper version can be named even though an earlier line parsed.
    forge_fiber_started_version(&dir, 1, &contract::SCHEMA_VERSION.to_string(), "x");
    assert_eq!(
        super::keeper_schema_version(&dir),
        None,
        "an unparseable latest clears an earlier version"
    );
    let keeper = log::Error::Held {
        session: id.0.clone(),
        holder: "keeper 4242".to_owned(),
    };
    let failure = super::keeper_refusal(&dir, &id, keeper);
    assert_eq!(failure.code, ErrorCode::SessionHeld);
    assert_eq!(
        failure.message,
        log::Error::Held {
            session: id.0.clone(),
            holder: "keeper 4242".to_owned(),
        }
        .to_string(),
        "no version to name keeps today's bare refusal"
    );
}

/// The hub path's `Held` decision beside the ask path's keeper decision
/// on one log: readable and forged-latest. Both call sites share the
/// helper, so a keeper-held lock declines identically through either.
fn parity_failures(sessions: &Path, id: &SessionId, dir: &Path) -> (Failure, Failure) {
    let held = || log::Error::Held {
        session: id.0.clone(),
        holder: "keeper 4242".to_owned(),
    };
    let hub = super::session_held_failure(sessions, id, held());
    let ask = super::keeper_refusal(dir, id, held());
    (hub, ask)
}

#[test]
fn keeper_schema_version_parity_across_call_sites() {
    let home = fakes::TempDir::new("main-resume-version-parity");
    let sessions = home.path().join("sessions");
    let id = SessionId("s_0123456789abcdef".into());
    let dir = sessions.join(&id.0);
    drop(session_with(&dir, None));
    // A keeper-held lock, as the resume's failed `Log::open` finds it.
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let _keeper = Log::open_as(&sessions, id.clone(), Arc::clone(&clock), "keeper 4242").unwrap();
    // Readable: both decline with today's bare refusal.
    let (hub, ask) = parity_failures(&sessions, &id, &dir);
    assert_eq!(hub, ask, "the call sites share the readable decision");
    assert_eq!(hub.code, ErrorCode::SessionHeld);
    // Forged latest: both decline naming the keeper's version.
    let next = u64::from(contract::SCHEMA_VERSION) + 1;
    forge_fiber_started_version(
        &dir,
        1,
        &contract::SCHEMA_VERSION.to_string(),
        &next.to_string(),
    );
    let (hub, ask) = parity_failures(&sessions, &id, &dir);
    assert_eq!(hub, ask, "the call sites share the forged-latest decision");
    assert_eq!(hub.code, ErrorCode::SessionHeld);
    assert!(
        hub.message.contains(&format!("runs schema version {next}")),
        "both name the keeper's version: {}",
        hub.message
    );
}

#[test]
fn keeper_schema_version_treats_zero_as_unreadable() {
    let home = fakes::TempDir::new("main-resume-version-zero");
    let sessions = home.path().join("sessions");
    let id = SessionId("s_0123456789abcdef".into());
    let dir = sessions.join(&id.0);
    let _log = session_with(&dir, None);
    forge_fiber_started_version(&dir, 1, &contract::SCHEMA_VERSION.to_string(), "0");
    assert_eq!(
        super::keeper_schema_version(&dir),
        Some(0),
        "the edge just below this build's version still reads"
    );
    let keeper = log::Error::Held {
        session: id.0.clone(),
        holder: "keeper 4242".to_owned(),
    };
    let failure = super::keeper_refusal(&dir, &id, keeper);
    assert_eq!(failure.code, ErrorCode::SessionHeld);
    assert!(
        failure.message.contains("runs schema version 0"),
        "version 0 declines naming 0: {}",
        failure.message
    );
}

#[test]
fn resume_start_rejects_a_log_from_another_session_directory() {
    let home = fakes::TempDir::new("main-resume-start-mismatch");
    let sessions = home.path().join("sessions");
    let log_dir = sessions.join("s_0123456789abcdef");
    let other_id = SessionId("s_0123456789abcde0".to_owned());
    let log = Arc::new(session_with(&log_dir, None));
    let folded = r#loop::resumed(log.dir()).unwrap();
    let dir = log::SessionDir::new(&sessions, &other_id);

    let result = ResumeStart::new(dir, log, folded, None);

    assert!(result.is_err());
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
                project: None,
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

// ---- the keeper's handover (`docs/tools.md`, "The keeper": a resume takes the log back) ----

/// How a scripted keeper behaves once the resume contacts it.
#[derive(Clone, Copy)]
enum Keeper {
    /// Answers as the session's keeper and hands its one job over.
    Hands,
    /// Answers for another session.
    Mistaken,
    /// Hands over, but another writer takes the released lock first.
    Beaten,
}

/// A keeper that holds the lock labelled as one, and speaks the handover on
/// the session's socket; the frames it received come back at the end.
fn scripted_keeper(
    home: &Path,
    sessions: &Path,
    id: &SessionId,
    behaviour: Keeper,
) -> std::thread::JoinHandle<(Vec<String>, Option<Log>)> {
    use std::io::{BufRead as _, BufReader, Write as _};
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let mut held =
        Some(Log::open_as(sessions, id.clone(), Arc::clone(&clock), "keeper 4242").unwrap());
    let (_, listener) = doors::bind(home, &sessions.join(&id.0)).unwrap();
    let (sessions, id) = (sessions.to_path_buf(), id.clone());
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        seen.push(line.trim().to_owned());
        let answered = if matches!(behaviour, Keeper::Mistaken) {
            "s_ffffffffffffffff".to_owned()
        } else {
            id.0.clone()
        };
        writeln!(writer, r#"{{"identity":{{"session_id":"{answered}"}}}}"#).unwrap();
        line.clear();
        if reader.read_line(&mut line).unwrap() > 0 {
            seen.push(line.trim().to_owned());
            // Released before the acknowledgement, as the keeper does.
            drop(held.take());
            held = matches!(behaviour, Keeper::Beaten)
                .then(|| Log::open_as(&sessions, id.clone(), clock, "other 1").unwrap());
            writeln!(
                writer,
                r#"{{"handover":{{"running":[{{"id":"call_0","job_id":"j_one","output_path":"/x","errors_path":null,"tty":false,"lines":false}}]}}}}"#
            )
            .unwrap();
            // The connection ends when the resume is done with it.
            line.clear();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                seen.push(line.trim().to_owned());
                line.clear();
            }
        }
        // Returned so the lock outlives the join: the test reads who holds it.
        (seen, held)
    })
}

fn bounded_open(
    home: PathBuf,
    sessions: PathBuf,
    id: SessionId,
    clock: Arc<dyn Clock>,
) -> (Result<(Log, Option<crate::keeper::Handover>), i32>, usize) {
    use std::sync::mpsc;
    use std::time::Duration;
    let (send, result) = mpsc::channel();
    std::thread::spawn(move || {
        let mut attached = 0;
        let opened = super::open_or_attach(&home, &sessions, id, clock, |_| {
            attached += 1;
            7
        });
        let _ = send.send((opened, attached));
    });
    fakes::Deadline::after(Duration::from_secs(10))
        .recv(&result)
        .expect("open_or_attach completes before its deadline")
}

fn bounded_keeper_join(
    keeper: std::thread::JoinHandle<(Vec<String>, Option<Log>)>,
) -> (Vec<String>, Option<Log>) {
    use std::sync::mpsc;
    use std::time::Duration;
    let (send, result) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(keeper.join());
    });
    fakes::Deadline::after(Duration::from_secs(10))
        .recv(&result)
        .expect("scripted keeper exits before its deadline")
        .expect("scripted keeper does not panic")
}

fn resume_fixture(name: &str) -> (fakes::TempDir, PathBuf, SessionId, PathBuf) {
    let home = fakes::TempDir::under(Path::new("/tmp"), name);
    let sessions = home.path().join("sessions");
    let id = SessionId("s_0123456789abcdef".into());
    let dir = sessions.join(&id.0);
    drop(session_with(&dir, None));
    (home, sessions, id, dir)
}

#[test]
fn a_readable_keeper_hands_over_instead_of_refusing() {
    let (home, sessions, id, dir) = resume_fixture("main-resume-handover");
    let keeper = scripted_keeper(home.path(), &sessions, &id, Keeper::Hands);
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let (opened, called) = bounded_open(home.path().to_path_buf(), sessions, id.clone(), clock);
    let attached = called;
    let Ok((log, Some(handover))) = opened else {
        panic!("a keeper that answers as the session hands the log over")
    };
    assert_eq!(attached, 0, "a keeper is not a live session to attach to");
    // The session holds the lock now, and the keeper's job comes with it.
    assert!(!log::lock_label(&dir).is_some_and(|label| label.starts_with("keeper ")));
    assert_eq!(handover.running().len(), 1);
    assert_eq!(handover.running()[0].job_id, JobId("j_one".into()));
    // The kernel names the keeper's process: here, the scripted one in-process.
    assert_eq!(handover.pid(), support::peer::Pid(std::process::id()));
    drop((log, handover));
    let (seen, _held) = bounded_keeper_join(keeper);
    assert_eq!(
        seen,
        [
            r#"{"identify":"s_0123456789abcdef"}"#,
            r#"{"handover":true}"#
        ]
    );
}

#[test]
fn a_keeper_that_answers_for_another_session_is_refused_and_keeps_the_lock() {
    let (home, sessions, id, dir) = resume_fixture("main-resume-mistaken");
    let keeper = scripted_keeper(home.path(), &sessions, &id, Keeper::Mistaken);
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let before = log::read(&dir).unwrap().len();
    let (result, attached) = bounded_open(home.path().to_path_buf(), sessions, id.clone(), clock);
    assert_eq!(result.err(), Some(1));
    assert_eq!(attached, 0);
    // Only the identity was asked for: no handover was requested, so the
    // keeper released nothing and still holds the log.
    let (seen, _held) = bounded_keeper_join(keeper);
    assert_eq!(seen, [r#"{"identify":"s_0123456789abcdef"}"#]);
    assert_eq!(log::lock_label(&dir).as_deref(), Some("keeper 4242"));
    assert_eq!(log::read(&dir).unwrap().len(), before, "nothing appended");
}

#[test]
fn a_lock_another_writer_takes_first_is_refused_held_and_the_connection_ends() {
    let (home, sessions, id, dir) = resume_fixture("main-resume-beaten");
    let keeper = scripted_keeper(home.path(), &sessions, &id, Keeper::Beaten);
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let before = log::read(&dir).unwrap().len();
    let (result, attached) = bounded_open(home.path().to_path_buf(), sessions, id.clone(), clock);
    assert_eq!(result.err(), Some(1));
    assert_eq!(attached, 0, "no attach to a lock only a keeper released");
    // The keeper saw both requests and then the end of the connection.
    let (seen, _held) = bounded_keeper_join(keeper);
    assert_eq!(seen.len(), 2);
    assert_eq!(log::lock_label(&dir).as_deref(), Some("other 1"));
    assert_eq!(log::read(&dir).unwrap().len(), before, "nothing appended");
}

#[test]
fn an_unreadable_keeper_version_is_declined_without_contacting_the_keeper() {
    let (home, sessions, id, dir) = resume_fixture("main-resume-unreadable");
    let next = u64::from(contract::SCHEMA_VERSION) + 1;
    forge_fiber_started_version(
        &dir,
        1,
        &contract::SCHEMA_VERSION.to_string(),
        &next.to_string(),
    );
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let _held = Log::open_as(&sessions, id.clone(), Arc::clone(&clock), "keeper 4242").unwrap();
    let (_, listener) = doors::bind(home.path(), &dir).unwrap();
    listener.set_nonblocking(true).unwrap();
    let result = super::open_or_attach(home.path(), &sessions, id.clone(), clock, |_| 7);
    assert_eq!(result.err(), Some(1));
    // Nobody connected: the keeper was not asked to release anything.
    assert_eq!(
        listener.accept().err().map(|error| error.kind()),
        Some(std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn a_resume_start_carries_a_handover_without_changing_its_construction() {
    let (_home, sessions, id, dir) = resume_fixture("main-resume-start");
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let log = Arc::new(Log::open(&sessions, id.clone(), clock).unwrap());
    let folded = r#loop::resumed(&dir).unwrap();
    let start = ResumeStart::new(log::SessionDir::new(&sessions, &id), log, folded, None)
        .unwrap()
        .with_handover(None);
    assert!(start.handover.is_none());
}
