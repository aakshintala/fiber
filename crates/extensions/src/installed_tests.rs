//! Tests for the healthy package directories: which `extensions/`
//! entries are kept, and in what order.

use std::fs;

use super::package_dirs;

/// A healthy install record, as `extensions/<dir>/.fiber.json` holds it.
const RECORD: &str = r#"{"name":"x","version":"1.0.0","requested":true,"source":{"path":"/p"}}"#;

/// Makes `dir` under `extensions/` hold a record with `text`.
fn write_record(home: &std::path::Path, dir: &str, text: &str) {
    let path = home.join("extensions").join(dir);
    fs::create_dir_all(&path).unwrap_or_else(|e| panic!("mkdir: {e}"));
    fs::write(path.join(".fiber.json"), text).unwrap_or_else(|e| panic!("write: {e}"));
}

/// Makes `dir` a healthy extension.
fn healthy(home: &std::path::Path, dir: &str) {
    write_record(home, dir, RECORD);
}

#[test]
fn a_missing_extensions_directory_gives_an_empty_list() {
    let home = fakes::TempDir::new("fiber-package-dirs-missing");
    assert!(package_dirs(home.path()).is_empty());
}

#[test]
fn an_extensions_path_that_is_a_file_gives_an_empty_list() {
    let home = fakes::TempDir::new("fiber-package-dirs-file");
    fs::write(home.path().join("extensions"), "nope").unwrap_or_else(|e| panic!("write: {e}"));
    assert!(package_dirs(home.path()).is_empty());
}

#[test]
fn healthy_directories_come_back_sorted_by_directory_name() {
    let home = fakes::TempDir::new("fiber-package-dirs-sorted");
    for dir in ["b", "a", "-local-path"] {
        healthy(home.path(), dir);
    }
    assert_eq!(
        package_dirs(home.path()),
        ["-local-path", "a", "b"]
            .into_iter()
            .map(|dir| home.path().join("extensions").join(dir))
            .collect::<Vec<_>>()
    );
}

#[test]
fn in_progress_damaged_and_plain_files_are_left_out() {
    let home = fakes::TempDir::new("fiber-package-dirs-skipped");
    healthy(home.path(), "a");
    write_record(home.path(), ".staging", RECORD);
    fs::create_dir_all(home.path().join("extensions").join("no-record"))
        .unwrap_or_else(|e| panic!("mkdir: {e}"));
    write_record(home.path(), "bad-record", "not json");
    fs::write(home.path().join("extensions").join("notes"), "nope")
        .unwrap_or_else(|e| panic!("write: {e}"));
    assert_eq!(
        package_dirs(home.path()),
        [home.path().join("extensions").join("a")]
    );
}
