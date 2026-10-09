//! Tests for the healthy package directories: which `extensions/`
//! entries are kept, and in what order.

use std::fs;

use super::{is_enabled, package_dirs, package_names};

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

#[cfg(target_os = "linux")]
#[test]
fn a_non_utf8_healthy_directory_is_returned_exactly_once() {
    use std::os::unix::ffi::OsStringExt as _;

    let home = fakes::TempDir::new("fiber-package-dirs-non-utf8");
    let raw = std::ffi::OsString::from_vec(b"ext-\xff".to_vec());
    let dir = home.path().join("extensions").join(&raw);
    // Linux only: macOS rejects non-UTF-8 file names (EILSEQ).
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
    fs::write(dir.join(".fiber.json"), RECORD).unwrap_or_else(|e| panic!("write: {e}"));
    assert_eq!(package_dirs(home.path()), vec![dir]);
}

#[test]
fn package_names_pairs_each_record_name_with_its_directory() {
    let home = fakes::TempDir::new("fiber-package-names");
    for (dir, name) in [("b", "bravo"), ("a", "alpha")] {
        let record = format!(
            r#"{{"name":"{name}","version":"1.0.0","requested":true,"source":{{"path":"/p"}}}}"#
        );
        write_record(home.path(), dir, &record);
    }
    assert_eq!(
        package_names(home.path()),
        [
            ("alpha".to_owned(), home.path().join("extensions").join("a")),
            ("bravo".to_owned(), home.path().join("extensions").join("b")),
        ]
    );
}

/// The configuration for `home` holding `text` as its global file.
fn config_with(home: &std::path::Path, text: &str) -> config::Config {
    std::fs::create_dir_all(home).unwrap_or_else(|e| panic!("mkdir: {e}"));
    std::fs::write(home.join("config.json"), text).unwrap_or_else(|e| panic!("write: {e}"));
    let project = config::ProjectKey::new("-w").unwrap_or_else(|err| panic!("key: {err}"));
    config::Config::load(config::Sources {
        home: home.to_path_buf(),
        workspace: home.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .unwrap_or_else(|err| panic!("config: {err}"))
}

#[test]
fn an_extension_is_enabled_unless_switched_off() {
    let home = fakes::TempDir::new("fiber-enabled-default");
    let config = config_with(home.path(), "{}");
    assert!(is_enabled(&config, "acme"));
}

#[test]
fn an_explicit_true_leaves_the_extension_enabled() {
    let home = fakes::TempDir::new("fiber-enabled-true");
    let config = config_with(
        home.path(),
        r#"{"extensions": {"acme": {"enabled": true}}}"#,
    );
    assert!(is_enabled(&config, "acme"));
}

#[test]
fn false_switches_the_extension_off() {
    let home = fakes::TempDir::new("fiber-enabled-false");
    let config = config_with(
        home.path(),
        r#"{"extensions": {"acme": {"enabled": false}}}"#,
    );
    assert!(!is_enabled(&config, "acme"));
}
