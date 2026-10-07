//! Tests for the diagnostics selection: the 30-day and newest-100 rule,
//! copied from the hub's pruning.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs::{self, File};
use std::time::{Duration, SystemTime};

use contract::clock::Clock;

use super::*;

fn wall() -> SystemTime {
    fakes::clock::FakeClock::new().wall()
}

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn write(dir: &Path, name: &str, bytes: &[u8]) {
    fs::write(dir.join(name), bytes).unwrap();
}

fn set_mtime(dir: &Path, name: &str, at: SystemTime) {
    File::options()
        .write(true)
        .open(dir.join(name))
        .unwrap()
        .set_modified(at)
        .unwrap();
}

fn paths(files: &[OldFile]) -> Vec<String> {
    let mut names: Vec<String> = files
        .iter()
        .map(|file| {
            file.path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn files_older_than_30_days_are_selected_and_the_boundary_is_kept() {
    let root = fakes::TempDir::new("cli-prune-diag");
    let dir = root.path().join("logs");
    fs::create_dir_all(&dir).unwrap();
    write(&dir, "old.log", b"old");
    write(&dir, "edge.log", b"edge");
    write(&dir, "fresh.log", b"fresh");
    set_mtime(
        &dir,
        "old.log",
        wall() - Duration::from_secs(30 * 24 * 60 * 60 + 1),
    );
    set_mtime(
        &dir,
        "edge.log",
        wall() - Duration::from_secs(30 * 24 * 60 * 60),
    );
    set_mtime(&dir, "fresh.log", wall() - DAY);
    assert_eq!(paths(&old_diagnostics(&dir, wall())), ["old.log"]);
}

#[test]
fn the_newest_100_files_by_mtime_are_kept() {
    let root = fakes::TempDir::new("cli-prune-diag-100");
    let dir = root.path().join("crashes");
    fs::create_dir_all(&dir).unwrap();
    for i in 0..101 {
        let name = format!("{i:03}.txt");
        write(&dir, &name, b"x");
        set_mtime(
            &dir,
            &name,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_000_000 + i),
        );
    }
    assert_eq!(paths(&old_diagnostics(&dir, wall())), ["000.txt"]);
}

#[test]
fn a_hundred_files_and_a_subdirectory_select_nothing() {
    let root = fakes::TempDir::new("cli-prune-diag-sub");
    let dir = root.path().join("logs");
    fs::create_dir_all(dir.join("sub")).unwrap();
    for i in 0..100 {
        let name = format!("{i:03}.log");
        write(&dir, &name, b"x");
        set_mtime(
            &dir,
            &name,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_000_000 + i),
        );
    }
    assert!(old_diagnostics(&dir, wall()).is_empty());
}

#[test]
fn a_symlink_is_never_selected() {
    let root = fakes::TempDir::new("cli-prune-diag-link");
    let dir = root.path().join("logs");
    fs::create_dir_all(&dir).unwrap();
    write(&dir, "old.log", b"old");
    set_mtime(
        &dir,
        "old.log",
        wall() - Duration::from_secs(31 * 24 * 60 * 60),
    );
    std::os::unix::fs::symlink(dir.join("old.log"), dir.join("link.log")).unwrap();
    assert_eq!(paths(&old_diagnostics(&dir, wall())), ["old.log"]);
}

#[test]
fn a_missing_directory_gives_an_empty_list() {
    let root = fakes::TempDir::new("cli-prune-diag-missing");
    assert!(old_diagnostics(&root.path().join("nope"), wall()).is_empty());
}

#[test]
fn results_are_sorted_by_path() {
    let root = fakes::TempDir::new("cli-prune-diag-sorted");
    let dir = root.path().join("logs");
    fs::create_dir_all(&dir).unwrap();
    for name in ["b.log", "a.log", "c.log"] {
        write(&dir, name, b"x");
        set_mtime(&dir, name, wall() - Duration::from_secs(31 * 24 * 60 * 60));
    }
    let files = old_diagnostics(&dir, wall());
    let names: Vec<String> = files
        .iter()
        .map(|file| {
            file.path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, ["a.log", "b.log", "c.log"]);
}

#[test]
fn bytes_is_each_files_length() {
    let root = fakes::TempDir::new("cli-prune-diag-bytes");
    let dir = root.path().join("logs");
    fs::create_dir_all(&dir).unwrap();
    write(&dir, "a.log", b"12345");
    write(&dir, "b.log", b"123");
    set_mtime(
        &dir,
        "a.log",
        wall() - Duration::from_secs(31 * 24 * 60 * 60),
    );
    set_mtime(
        &dir,
        "b.log",
        wall() - Duration::from_secs(31 * 24 * 60 * 60),
    );
    let mut files = old_diagnostics(&dir, wall());
    files.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].bytes, 5);
    assert_eq!(files[1].bytes, 3);
}
