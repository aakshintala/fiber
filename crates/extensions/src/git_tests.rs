//! Naming a repository and the directory inside it (`docs/extensions.md`,
//! "Names").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

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
    let err = Origin::new(missing.to_string_lossy().into_owned(), |repo| repo.to_owned())
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
