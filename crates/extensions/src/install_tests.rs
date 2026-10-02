//! The swap that puts a fresh copy in place, with renames that fail on
//! purpose.

use std::cell::Cell;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
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
        assert!(!dirs.root.join(".commit").exists());
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
    assert!(!dirs.root.join(".commit").exists());
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
    // `a` was removed and the restore rename failed, so the journal remains.
    assert!(dirs.root.join(".commit").is_file());
    assert!(!dirs.path("a-target").exists());
    let b = fs::read_to_string(dirs.path("b-target/v")).unwrap();
    assert_eq!(b, "old");
    super::recover(&dirs.root).unwrap();
    assert_eq!(fs::read_to_string(dirs.path("a-target/v")).unwrap(), "old");
    assert_eq!(fs::read_to_string(dirs.path("b-target/v")).unwrap(), "old");
    assert!(!dirs.path("a-old").exists());
    assert!(!dirs.root.join(".commit").exists());
}

#[test]
fn a_copy_whose_own_put_back_fails_is_put_back_by_the_rollback() {
    // Moves: a aside, a in place, b aside, b in place (fails), b back
    // (fails), then the rollback: a back, b back.
    let (dirs, paths) = pair("own-put-back");
    let calls = Cell::new(0);
    commit_all(&paths, |from, to| {
        calls.set(calls.get() + 1);
        if matches!(calls.get(), 4 | 5) {
            Err(io::Error::other("injected"))
        } else {
            fs::rename(from, to)
        }
    })
    .unwrap_err();
    for n in ["a", "b"] {
        let v = fs::read_to_string(dirs.path(&format!("{n}-target/v"))).unwrap();
        assert_eq!(v, "old", "{n}");
        assert!(!dirs.path(&format!("{n}-old")).exists(), "{n}");
    }
    assert!(!dirs.root.join(".commit").exists());
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

fn journal(dir: &Path, committed: bool, steps: serde_json::Value) {
    fs::write(
        dir.join(".commit"),
        serde_json::json!({ "committed": committed, "steps": steps }).to_string(),
    )
    .unwrap();
}

fn step<'a>(target: &'a Path, old: &'a Path, fresh: &'a Path, had_old: bool) -> serde_json::Value {
    serde_json::json!({
        "target": target.display().to_string(),
        "old": old.display().to_string(),
        "fresh": fresh.display().to_string(),
        "had_old": had_old,
    })
}

#[test]
fn an_uncommitted_journal_removes_a_new_install_and_restores_a_replacement() {
    let dirs = Dirs::new("journal-abort");
    let installed = dirs.path("installed");
    let backup = dirs.path("backup");
    let leftover = dirs.path("leftover");
    fs::create_dir_all(&installed).unwrap();
    fs::write(installed.join("v"), "new").unwrap();
    fs::create_dir_all(&leftover).unwrap();
    let replaced = dirs.path("replaced");
    fs::create_dir_all(&replaced).unwrap();
    fs::write(replaced.join("v"), "new").unwrap();
    fs::create_dir_all(&backup).unwrap();
    fs::write(backup.join("v"), "old").unwrap();
    fs::create_dir_all(dirs.path("replaced-fresh")).unwrap();
    journal(
        &dirs.root,
        false,
        serde_json::json!([
            step(&installed, &dirs.path("no-backup"), &leftover, false),
            step(&replaced, &backup, &dirs.path("replaced-fresh"), true),
        ]),
    );
    super::recover(&dirs.root).unwrap();
    assert!(!installed.exists(), "a new install is removed");
    assert!(!leftover.exists());
    assert_eq!(fs::read_to_string(replaced.join("v")).unwrap(), "old");
    assert!(!backup.exists());
    assert!(!dirs.path("replaced-fresh").exists());
    assert!(!dirs.root.join(".commit").exists());
}

fn set_mode(path: &Path, mode: u32) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).unwrap();
}

/// Puts the directory back to `0o755` so the temporary tree can be removed.
struct Open(PathBuf);

impl Drop for Open {
    fn drop(&mut self) {
        if fs::metadata(&self.0).is_ok() {
            set_mode(&self.0, 0o755);
        }
    }
}

#[test]
fn a_target_that_cannot_be_stated_stops_before_any_move() {
    let (dirs, paths) = pair("meta");
    let _open = Open(dirs.root.clone());
    set_mode(&dirs.root, 0);
    let err = commit_all(&paths, |from, to| fs::rename(from, to)).unwrap_err();
    set_mode(&dirs.root, 0o755);
    assert!(err.to_string().contains("a-target"), "{err}");
    assert!(!dirs.root.join(".commit").exists());
    assert_eq!(fs::read_to_string(dirs.path("a-target/v")).unwrap(), "old");
}

#[test]
fn a_path_whose_metadata_fails_is_an_error_naming_it() {
    let dirs = Dirs::new("exists");
    let hidden = dirs.path("hidden");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("f"), "x").unwrap();
    let _open = Open(hidden.clone());
    set_mode(&hidden, 0);
    let err = super::exists(&hidden.join("f")).unwrap_err();
    set_mode(&hidden, 0o755);
    assert!(
        err.to_string().contains("hidden/f") || err.to_string().contains("f"),
        "{err}"
    );
}

#[test]
fn a_journal_that_cannot_be_read_is_an_error_naming_it() {
    let dirs = Dirs::new("journal-dir");
    fs::create_dir(dirs.root.join(".commit")).unwrap();
    let Err(err) = super::read_journal(&dirs.root) else {
        panic!("a directory was read as a journal");
    };
    assert!(err.to_string().contains(".commit"), "{err}");
}

#[test]
fn removing_a_missing_journal_succeeds() {
    let dirs = Dirs::new("journal-missing");
    super::remove_journal(&dirs.root).unwrap();
}

#[test]
fn removing_a_journal_that_is_a_directory_fails_naming_it() {
    let dirs = Dirs::new("journal-not-file");
    fs::create_dir(dirs.root.join(".commit")).unwrap();
    let err = super::remove_journal(&dirs.root).unwrap_err();
    assert!(err.to_string().contains(".commit"), "{err}");
    assert!(dirs.root.join(".commit").is_dir());
}

#[test]
fn a_committed_journal_keeps_the_new_copy_and_drops_the_backup() {
    let dirs = Dirs::new("journal-commit");
    let target = dirs.path("target");
    let backup = dirs.path("backup");
    let fresh = dirs.path("fresh-left");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("v"), "new").unwrap();
    fs::create_dir_all(&backup).unwrap();
    fs::write(backup.join("v"), "old").unwrap();
    fs::create_dir_all(&fresh).unwrap();
    journal(
        &dirs.root,
        true,
        serde_json::json!([step(&target, &backup, &fresh, true)]),
    );
    super::recover(&dirs.root).unwrap();
    assert_eq!(fs::read_to_string(target.join("v")).unwrap(), "new");
    assert!(!backup.exists());
    assert!(!fresh.exists());
    assert!(!dirs.root.join(".commit").exists());
}
