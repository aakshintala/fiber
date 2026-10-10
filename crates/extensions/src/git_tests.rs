//! Naming a repository and the directory inside it (`docs/extensions.md`,
//! "Names").

use super::{is_path, split};
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
