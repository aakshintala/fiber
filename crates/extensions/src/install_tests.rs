//! The swap that puts a fresh copy in place, with renames that fail on
//! purpose.

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Paths, commit_all, swap};

struct Dirs {
    root: PathBuf,
}

impl Dirs {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("fiber-swap-{}-{name}", std::process::id()));
        fs::remove_dir_all(&root).unwrap_or(());
        fs::create_dir_all(root.join("fresh")).unwrap();
        fs::write(root.join("fresh/v"), "new").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/v"), "old").unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn swap(&self, fail_on: usize) -> Result<(), crate::Error> {
        let calls = Cell::new(0);
        swap(
            &self.path("fresh"),
            &self.path("target"),
            &self.path("old"),
            |from: &Path, to: &Path| {
                calls.set(calls.get() + 1);
                if calls.get() == fail_on {
                    Err(io::Error::other("injected"))
                } else {
                    fs::rename(from, to)
                }
            },
        )
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap_or(());
    }
}

#[test]
fn a_swap_puts_the_fresh_copy_in_place() {
    let dirs = Dirs::new("ok");
    dirs.swap(0).unwrap();
    assert_eq!(fs::read_to_string(dirs.path("target/v")).unwrap(), "new");
    assert!(!dirs.path("fresh").exists());
}

#[test]
fn a_failed_move_aside_leaves_the_installed_copy() {
    let dirs = Dirs::new("aside");
    dirs.swap(1).unwrap_err();
    assert_eq!(fs::read_to_string(dirs.path("target/v")).unwrap(), "old");
}

#[test]
fn a_failed_promotion_restores_the_installed_copy() {
    let dirs = Dirs::new("promote");
    let err = dirs.swap(2).unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::IoFailed);
    assert_eq!(fs::read_to_string(dirs.path("target/v")).unwrap(), "old");
    assert!(!dirs.path("old").exists());
    assert_eq!(fs::read_to_string(dirs.path("fresh/v")).unwrap(), "new");
}

#[test]
fn a_first_install_has_nothing_to_move_aside() {
    let dirs = Dirs::new("first");
    fs::remove_dir_all(dirs.path("target")).unwrap();
    dirs.swap(2).unwrap();
    assert_eq!(fs::read_to_string(dirs.path("target/v")).unwrap(), "new");
}

/// Two staged copies, `a` and `b`, each replacing an installed one.
fn pair(name: &str) -> (Dirs, [Paths; 2]) {
    let dirs = Dirs::new(name);
    let make = |n: &str| {
        fs::create_dir_all(dirs.path(&format!("{n}-fresh"))).unwrap();
        fs::write(dirs.path(&format!("{n}-fresh/v")), "new").unwrap();
        fs::create_dir_all(dirs.path(&format!("{n}-target"))).unwrap();
        fs::write(dirs.path(&format!("{n}-target/v")), "old").unwrap();
        Paths {
            fresh: dirs.path(&format!("{n}-fresh")),
            target: dirs.path(&format!("{n}-target")),
            old: dirs.path(&format!("{n}-old")),
        }
    };
    let paths = [make("a"), make("b")];
    (dirs, paths)
}

#[test]
fn a_failed_second_move_puts_the_first_copy_back() {
    // Moves: a aside, a in place, b aside, b in place.
    for fail_on in [3, 4] {
        let (dirs, paths) = pair(&format!("commit-{fail_on}"));
        let calls = Cell::new(0);
        let err = commit_all(&paths, |from, to| {
            calls.set(calls.get() + 1);
            if calls.get() == fail_on {
                Err(io::Error::other("injected"))
            } else {
                fs::rename(from, to)
            }
        })
        .unwrap_err();
        assert_eq!(err.code(), contract::ErrorCode::IoFailed);
        for n in ["a", "b"] {
            let v = fs::read_to_string(dirs.path(&format!("{n}-target/v"))).unwrap();
            assert_eq!(v, "old", "{n} after failing move {fail_on}");
            assert!(!dirs.path(&format!("{n}-fresh")).exists(), "{n}");
        }
    }
}

#[test]
fn every_staged_copy_goes_in_place_and_nothing_is_left_beside() {
    let (dirs, paths) = pair("commit-ok");
    commit_all(&paths, |from, to| fs::rename(from, to)).unwrap();
    for n in ["a", "b"] {
        let v = fs::read_to_string(dirs.path(&format!("{n}-target/v"))).unwrap();
        assert_eq!(v, "new");
        assert!(!dirs.path(&format!("{n}-old")).exists());
    }
}

#[test]
fn a_copy_that_cannot_be_put_back_is_an_error_naming_it() {
    // Moves: a aside, a in place, b aside (fails), then a's old copy back
    // (fails).
    let (dirs, paths) = pair("stuck");
    let calls = Cell::new(0);
    let err = commit_all(&paths, |from, to| {
        calls.set(calls.get() + 1);
        if matches!(calls.get(), 3 | 4) {
            Err(io::Error::other("injected"))
        } else {
            fs::rename(from, to)
        }
    })
    .unwrap_err();
    let crate::Error::Rollback { why, stuck } = &err else {
        panic!("{err}")
    };
    assert!(why.contains("injected"), "{why}");
    assert_eq!(stuck, &[dirs.path("a-target")]);
    assert_eq!(err.code(), contract::ErrorCode::IoFailed);
    assert!(err.to_string().contains("a-target"), "{err}");
    // What could be put back was: b was never moved.
    let b = fs::read_to_string(dirs.path("b-target/v")).unwrap();
    assert_eq!(b, "old");
}

#[test]
fn staging_directories_differ_by_plan_id() {
    use super::{Provenance, Record, stage};
    let dirs = Dirs::new("ids");
    let source = dirs.path("src");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("extension.json"),
        r#"{"name":"acme","version":"v1","fiber":"0.1.0","api":1}"#,
    )
    .unwrap();
    let record = Record {
        name: "acme".into(),
        provenance: Provenance::Path(source.clone()),
        version: "v1".into(),
        requested: true,
    };
    let home = dirs.path("home");
    let one = stage(&home, 1, &source, "0.1.0", &record).unwrap().2;
    let two = stage(&home, 2, &source, "0.1.0", &record).unwrap().2;
    assert_ne!(one.fresh, two.fresh);
    assert!(one.fresh.is_dir() && two.fresh.is_dir());
}
