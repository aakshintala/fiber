//! Naming a repository and the directory inside it (`docs/extensions.md`,
//! "Names").

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use super::{Origin, is_path, split};
use crate::Error;

#[test]
fn a_path_is_told_from_a_name() {
    for path in ["./tools/lint", "../lint", "/abs/lint", "~/lint"] {
        assert!(is_path(path), "{path}");
    }
    for name in ["muse", "github.com/acme/lint"] {
        assert!(!is_path(name), "{name}");
    }
}

#[test]
fn a_name_splits_into_repository_and_directory() {
    assert_eq!(split("github.com/a/b").unwrap(), ("github.com/a/b", ""));
    assert_eq!(
        split("github.com/a/b/p/q").unwrap(),
        ("github.com/a/b", "p/q")
    );
    assert_eq!(
        split("gitlab.com/g/s/repo.git/p/q").unwrap(),
        ("gitlab.com/g/s/repo.git", "p/q")
    );
    assert_eq!(
        split("gitlab.com/g/s/repo.git").unwrap(),
        ("gitlab.com/g/s/repo.git", "")
    );
    assert_eq!(
        split("github.com/a/b.git/p").unwrap(),
        ("github.com/a/b.git", "p")
    );
    assert_eq!(split("github.com/a/b/p").unwrap(), ("github.com/a/b", "p"));
    // The first `.git` segment ends the repository; a later one is a directory.
    assert_eq!(
        split("gitlab.com/g/s/repo.git/nested.git/p").unwrap(),
        ("gitlab.com/g/s/repo.git", "nested.git/p")
    );
    assert_eq!(
        split("github.com/a/x.git.git").unwrap(),
        ("github.com/a/x.git.git", "")
    );
    // A bare `.git` segment is not a marker.
    assert_eq!(
        split("gitlab.com/g/s/.git/p").unwrap(),
        ("gitlab.com/g/s", ".git/p")
    );
    // `repo.gitx` is not a marker, so the first three parts stay the repository.
    assert_eq!(
        split("github.com/a/repo.gitx/p").unwrap(),
        ("github.com/a/repo.gitx", "p")
    );
    assert_eq!(split("github.com/a/b/").unwrap(), ("github.com/a/b", ""));
    for bad in [
        "muse",
        "a/b",
        "a//b",
        "/a/b",
        "a/b.git/p",
        "gitlab.com//repo.git/p",
        ".git/p",
        "github.com/a.git/b",
    ] {
        assert!(matches!(split(bad), Err(Error::BadName { .. })), "{bad}");
    }
}

/// A `git` that cannot start fails the call: a missing program is
/// `GitMissing`, anything else names the spawn error.
#[test]
fn a_git_that_cannot_start_fails_the_call() {
    use std::os::unix::fs::PermissionsExt;

    let dir = fakes::TempDir::new("fiber-git-spawn");
    let missing = dir.path().join("fiber-definitely-missing-xyz");
    let clock = fakes::clock::FakeClock::new();
    let err = Origin::new(missing.to_string_lossy().into_owned(), |repo| {
        repo.to_owned()
    })
    .tags("github.com/acme/x", clock.as_ref())
    .unwrap_err();
    assert!(
        matches!(err, crate::Error::GitMissing),
        "a missing git is GitMissing: {err}"
    );

    let blocked = dir.path().join("not-executable");
    std::fs::write(&blocked, "x").unwrap();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = Origin::new(blocked.to_string_lossy().into_owned(), |repo| {
        repo.to_owned()
    })
    .tags("github.com/acme/x", clock.as_ref())
    .unwrap_err();
    assert!(
        matches!(&err, crate::Error::Git { command, .. } if command == "ls-remote --tags --refs github.com/acme/x"),
        "an unstartable git fails the call: {err}"
    );
    assert!(
        err.to_string().contains("denied"),
        "the failure carries the spawn error: {err}"
    );
}

/// A `diff --stat` that never answers fails at the git deadline instead of
/// standing in a change list it never read: the timeout reaches the caller,
/// never the "not in the repository" fallback.
#[test]
fn a_stalled_diff_fails_at_the_git_deadline() {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use fakes::clock::FakeClock;

    use super::GIT_DEADLINE;
    use crate::host::exec::{GRACE, GROUP_POLL};
    use contract::clock::Clock as _;

    const WITHIN: Duration = fakes::MUST_SUCCEED_WITHIN;
    /// One sighting round's wait for the run's answer.
    const SIGHT: Duration = Duration::from_millis(200);
    /// Rounds of waiting for the stalled process to prove it is stuck: 8
    /// rounds of two bounded waits are about 3 s of wall clock, the hang
    /// guard for a stall that never appears.
    const STUCK_ROUNDS: u32 = 8;

    /// Waits until the process holding `stall` on its command line has
    /// outlived a bounded wait, returning an early answer at once. A process
    /// seen running across the wait is stuck, not starting: stopping a
    /// starter reports a timeout the stall never caused, so the clock moves
    /// only after the second sighting. A stall that answers instead fails on
    /// its own answer.
    fn await_stuck<T: Send>(done: &mpsc::Receiver<T>, stall: &str) -> Option<T> {
        for _ in 0..STUCK_ROUNDS {
            if !fakes::matching(stall).unwrap().is_empty() {
                // Up: still up after a bounded wait means stuck, not
                // starting; an answer meanwhile ends this at once.
                match done.recv_timeout(SIGHT) {
                    Ok(done) => return Some(done),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        panic!("the stalled run returns")
                    }
                }
                if !fakes::matching(stall).unwrap().is_empty() {
                    return None;
                }
            }
            match done.recv_timeout(SIGHT) {
                Ok(done) => return Some(done),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the stalled run returns")
                }
            }
        }
        panic!("the stalled process runs");
    }

    let dir = fakes::TempDir::new("fiber-git-diff-stall");
    // The old commit carries the stall marker, as the installed commit
    // would; the fixture keeps it in the stall's argv, so the stalled
    // process matches this run alone.
    let unique = format!("stall-diff-{}", dir.path().display());
    let watching = unique.clone();
    let git = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-git/git")
        .to_string_lossy()
        .into_owned();
    let clock = FakeClock::new();
    let worker_clock = Arc::clone(&clock);
    let root = dir.path().to_path_buf();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("stalled diff".into())
        .spawn(move || {
            let origin = Origin::new(git, |repo| repo.to_owned());
            let _sent = done_tx.send(origin.changes(&root, &unique, "", &*worker_clock));
        })
        .unwrap();
    assert!(
        clock.await_parked(clock.now() + GROUP_POLL, WITHIN),
        "waited {WITHIN:?} for diff to park while running"
    );
    // The stall proves it is stuck before the clock first moves: stopping
    // a starter on the way up reports a timeout the stall never caused.
    let early = await_stuck(&done_rx, &watching);
    assert!(early.is_none(), "the stall answers only at its deadline");
    // Past the git deadline the run stops.
    clock.advance(GIT_DEADLINE + Duration::from_secs(1));
    let kill_at = clock.now() + GRACE;
    assert!(
        clock.await_parked(kill_at, WITHIN),
        "waited {WITHIN:?} for diff to park for the grace"
    );
    clock.advance(GRACE);
    let err = done_rx.recv_timeout(WITHIN).unwrap().unwrap_err();
    assert!(
        matches!(&err, Error::Git { .. }),
        "a stalled diff fails as git failed: {err}"
    );
    assert!(
        err.to_string().contains(&format!(
            "did not finish within {} s",
            GIT_DEADLINE.as_secs()
        )),
        "the failure names the deadline's seconds: {err}"
    );
    assert!(
        err.to_string().contains("so it was stopped"),
        "the failure names the stop: {err}"
    );
    // The run kills what it stopped, so leftovers fail the test without
    // leaking. By pid, never by group: the stall shares the test's group.
    let leftovers = fakes::matching(&watching).unwrap();
    for pid in &leftovers {
        drop(fakes::kill_pid(*pid, "KILL"));
    }
    assert!(
        leftovers.is_empty(),
        "the stalled diff is gone: {leftovers:?}"
    );
}

/// A `diff` that fails at once keeps the missing-commit fallback: only a
/// deadline timeout reaches the caller.
#[test]
fn a_failed_diff_keeps_the_missing_commit_fallback() {
    let dir = fakes::TempDir::new("fiber-git-diff-fallback");
    let clock = fakes::clock::FakeClock::new();
    // No `git init`: the call fails before any deadline could pass, on a
    // clock that never moves.
    let origin = Origin::new("git", |repo| repo.to_owned());
    let changes = origin
        .changes(
            dir.path(),
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "",
            &*clock,
        )
        .unwrap();
    assert_eq!(
        changes, "The installed commit is not in the repository.\n",
        "a failed diff stands in the missing-commit text"
    );
}
