//! Naming a repository and the directory inside it (`docs/extensions.md`,
//! "Names").

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use super::{Origin, is_path, split};
use crate::Error;
use fakes::Deadline;

use super::run_git;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

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
    use contract::clock::Clock as _;
    use support::group::{GRACE, GROUP_POLL};

    const WITHIN: Duration = fakes::MUST_SUCCEED_WITHIN;
    /// One sighting round's wait for the run's answer: the poll pause
    /// between checks, never a fresh deadline. The whole wait below ends at
    /// one 10 s wall-clock deadline made before its first check.
    const SIGHT: Duration = Duration::from_millis(200);

    /// Waits until the process holding `stall` on its command line has
    /// outlived a bounded wait, returning an early answer at once. A process
    /// seen running across the wait is stuck, not starting: stopping a
    /// starter reports a timeout the stall never caused, so the clock moves
    /// only after the second sighting. A stall that answers instead fails on
    /// its own answer.
    #[track_caller]
    fn await_stuck<T: Send>(done: &mpsc::Receiver<T>, stall: &str) -> Option<T> {
        let bound = Deadline::after(Duration::from_secs(10));
        loop {
            if !fakes::matching(stall).unwrap().is_empty() {
                // Up: still up after a bounded wait means stuck, not
                // starting; an answer meanwhile ends this at once.
                match Deadline::after(SIGHT).recv(done) {
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
            if bound.left().is_zero() {
                break;
            }
            match Deadline::after(SIGHT).recv(done) {
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
    // The guard matches this stall alone by its argv: a panic anywhere
    // below still kills it, by pid and never the test's own group.
    let watchdog = fakes::Watchdog::matching(&watching);
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
        clock.await_parked(clock.now() + GROUP_POLL, fakes::Deadline::after(WITHIN)),
        "waited {WITHIN:?} for diff to park while running"
    );
    // The stall proves it is stuck before the clock first moves: stopping
    // a starter on the way up reports a timeout the stall never caused.
    let early = await_stuck(&done_rx, &watching);
    assert!(early.is_none(), "the stall answers only at its deadline");
    // At the git deadline the run stops: it parks for the grace, or
    // answers at once when the stall dies on SIGTERM with no grace park.
    // Crosses only poll pauses to the git deadline; the run is proven parked at its poll (ruling 6260728504).
    #[allow(
        clippy::disallowed_methods,
        reason = "poll-pause jump: proven parked at its poll, crossing only poll pauses and the deadline (ruling 6260728504)"
    )]
    clock.advance(GIT_DEADLINE);
    let kill_at = clock.now() + GRACE;
    let answered = crate::stall::await_grace_or_answer(&clock, &done_rx, kill_at);
    let answered =
        crate::stall::advance_past_grace_or_answer(&clock, &done_rx, kill_at, GRACE, answered);
    let answer = match answered {
        Some(answer) => answer,
        None => Deadline::after(WITHIN).recv(&done_rx).unwrap(),
    };
    let err = answer.unwrap_err();
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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

/// A repository with a lightweight tag `v1`, an annotated tag `v1-annotated`
/// (whose peeled `^{}` line must never match) and a `v10` that must not
/// match a `v1` lookup, served over `file://`.
fn tagged_repo() -> (fakes::TempDir, Origin) {
    let repo = fakes::git::repo("fiber-git-tag-commit");
    fakes::git::git(repo.path(), &["tag", "v1"]);
    fakes::git::git(repo.path(), &["tag", "-a", "-m", "ten", "v10"]);
    fakes::git::git(
        repo.path(),
        &["tag", "-a", "-m", "annotated", "v1-annotated"],
    );
    let path = repo.path().to_owned();
    let origin = Origin::new("git", move |_| format!("file://{}", path.display()));
    (repo, origin)
}

#[test]
fn tag_commit_returns_the_exact_tag_line_only() {
    let (_repo, origin) = tagged_repo();
    let clock = fakes::clock::FakeClock::new();
    let sha = origin.tag_commit("x", "v1", clock.as_ref()).unwrap();
    assert_eq!(sha.len(), 40, "{sha}");
    assert_eq!(sha, origin.tag_commit("x", "v1", clock.as_ref()).unwrap());
    // `v10` is its own tag: the `v1` lookup never matches it.
    let other = origin.tag_commit("x", "v10", clock.as_ref()).unwrap();
    assert_eq!(other.len(), 40, "{other}");
    // An annotated tag resolves to its tag line, never the peeled commit.
    let annotated = origin
        .tag_commit("x", "v1-annotated", clock.as_ref())
        .unwrap();
    assert_eq!(annotated.len(), 40, "{annotated}");
    assert_ne!(annotated, other);
}

#[test]
fn tag_commit_matches_the_whole_tag_only() {
    let (_repo, origin) = tagged_repo();
    let clock = fakes::clock::FakeClock::new();
    // `v1-annotated` exists, but `v1-annotate` is no tag: a prefix match
    // would wrongly return the longer tag's SHA.
    let err = origin
        .tag_commit("github.com/acme/x", "v1-annotate", clock.as_ref())
        .unwrap_err();
    let Error::MissingTag { repo, tag } = &err else {
        panic!("a prefix of a tag is a missing tag: {err}");
    };
    assert_eq!(repo, "github.com/acme/x");
    assert_eq!(tag, "v1-annotate");
}

#[test]
fn tag_commit_names_a_missing_tag_and_its_repository() {
    let (_repo, origin) = tagged_repo();
    let clock = fakes::clock::FakeClock::new();
    let err = origin
        .tag_commit("github.com/acme/x", "v9.9.9", clock.as_ref())
        .unwrap_err();
    assert!(err.to_string().contains("v9.9.9"), "{err}");
    assert!(err.to_string().contains("github.com/acme/x"), "{err}");
}

/// The fake `git` answering `ls-remote`, so a failed listing names the
/// repository the URL asked for.
fn failing_git(url: String) -> Origin {
    let git = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-git/git")
        .to_string_lossy()
        .into_owned();
    Origin::new(git, move |_| url.clone())
}

/// `ls-remote` failing with a missing-repository error maps to
/// `NoRepository`, naming the repository: the guard's true arm.
#[test]
fn tag_commit_reports_a_missing_repository() {
    let clock = fakes::clock::FakeClock::new();
    let err = failing_git("https://fake-missing-repo/x".to_owned())
        .tag_commit("github.com/acme/x", "v1", clock.as_ref())
        .unwrap_err();
    let Error::NoRepository { name, .. } = &err else {
        panic!("a missing repository is NoRepository: {err}");
    };
    assert_eq!(name, "github.com/acme/x");
}

/// `ls-remote` failing any other way stays a `Git` failure: the guard's
/// false arm. An authentication failure is not a missing repository.
#[test]
fn tag_commit_keeps_a_non_missing_git_failure() {
    let clock = fakes::clock::FakeClock::new();
    let err = failing_git("https://fake-broken-repo/x".to_owned())
        .tag_commit("github.com/acme/x", "v1", clock.as_ref())
        .unwrap_err();
    assert!(
        matches!(&err, Error::Git { .. }),
        "a non-missing failure stays git: {err}"
    );
}

/// The child re-run markers for the redirect test.
const REDIRECT_CHILD: &str = "FIBER_EXTENSIONS_GIT_REDIRECT_CHILD";
const REDIRECT_REPO: &str = "FIBER_EXTENSIONS_GIT_REDIRECT_REPO";

/// This test binary's path of `test`: `module_path!` carries the crate
/// name, which is not part of a test's path.
fn this(test: &str) -> String {
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{module}::{test}")
}

/// A `GIT_DIR` pointing at a decoy repository does not reach `run_git`:
/// `rev-parse --absolute-git-dir` still prints the repository's own
/// directory, resolved as git prints it.
#[test]
fn a_redirected_git_dir_does_not_reach_run_git() {
    if std::env::var(REDIRECT_CHILD).is_ok() {
        let repo = PathBuf::from(std::env::var(REDIRECT_REPO).unwrap());
        let clock = fakes::clock::FakeClock::new();
        let ran = run_git(
            "git",
            &["rev-parse", "--absolute-git-dir"],
            &repo,
            clock.as_ref(),
        )
        .unwrap();
        assert_eq!(ran.exit_code, Some(0));
        assert_eq!(
            PathBuf::from(String::from_utf8_lossy(&ran.stdout).trim().to_owned()),
            fs::canonicalize(repo.join(".git")).unwrap()
        );
        return;
    }
    let temp = fakes::TempDir::new("extensions-git-redirect");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fakes::git::git(&repo, &["init", "--quiet"]);
    let decoy = temp.path().join("decoy");
    fs::create_dir_all(&decoy).unwrap();
    fakes::git::git(&decoy, &["init", "--quiet"]);
    let out = fakes::rerun_prepared(
        &this("a_redirected_git_dir_does_not_reach_run_git"),
        &[
            (REDIRECT_CHILD, "1"),
            (REDIRECT_REPO, repo.to_str().unwrap()),
            ("GIT_DIR", decoy.join(".git").to_str().unwrap()),
        ],
        Deadline::after(Duration::from_secs(60)),
        |_| {},
    );
    assert!(
        out.status.success(),
        "run_git must see past GIT_DIR:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
