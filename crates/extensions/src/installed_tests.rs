//! Tests for the healthy package directories: which `extensions/`
//! entries are kept, and in what order.

use std::fs;

use super::{is_enabled, package_names, read};
use fakes::Deadline;

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
    assert!(package_names(home.path()).is_empty());
}

#[test]
fn an_extensions_path_that_is_a_file_gives_an_empty_list() {
    let home = fakes::TempDir::new("fiber-package-dirs-file");
    fs::write(home.path().join("extensions"), "nope").unwrap_or_else(|e| panic!("write: {e}"));
    assert!(package_names(home.path()).is_empty());
}

#[test]
fn healthy_directories_come_back_sorted_by_directory_name() {
    let home = fakes::TempDir::new("fiber-package-dirs-sorted");
    for dir in ["b", "a", "-local-path"] {
        healthy(home.path(), dir);
    }
    assert_eq!(
        package_names(home.path()),
        ["-local-path", "a", "b"]
            .into_iter()
            .map(|dir| ("x".to_owned(), home.path().join("extensions").join(dir)))
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
        package_names(home.path()),
        [("x".to_owned(), home.path().join("extensions").join("a"))]
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
    assert_eq!(package_names(home.path()), vec![("x".to_owned(), dir)]);
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
fn enabled_lookup_addresses_quote_names_without_dotted_key_parsing() {
    fakes::within(
        "enabled quote-name lookup",
        Deadline::after(std::time::Duration::from_secs(10)),
        || {
            let home = fakes::TempDir::new("fiber-enabled-quote");
            for enabled in [false, true] {
                let text = serde_json::json!({"extensions": {"ac\"me": {"enabled":enabled}, "opencode": {"enabled":enabled}}}).to_string();
                let config = config_with(home.path(), &text);
                assert_eq!(is_enabled(&config, "ac\"me"), enabled);
                assert_eq!(is_enabled(&config, "opencode"), enabled);
                assert_eq!(
                    is_enabled(&config, "github.com/aakshintala/fiber/providers/opencode"),
                    enabled
                );
                assert!(is_enabled(&config, "unset\"name"));
            }
        },
    );
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

#[test]
#[allow(clippy::unwrap_used, reason = "a failure is the test's")]
fn a_missing_extensions_directory_lists_nothing() {
    let home = fakes::TempDir::new("fiber-read-missing");
    let listing = read(home.path()).unwrap();
    assert!(listing.installed.is_empty());
    assert!(listing.damaged.is_empty());
}

#[test]
#[allow(clippy::unwrap_used, reason = "a failure is the test's")]
fn an_extensions_path_that_is_not_a_directory_is_an_error() {
    let home = fakes::TempDir::new("fiber-read-file");
    fs::write(home.path().join("extensions"), "nope").unwrap();
    let err = read(home.path()).unwrap_err();
    assert!(err.to_string().contains("extensions"), "{err}");
}

/// A healthy extension directory: `extension.json` naming `name` with
/// `depends`, beside the record `text`.
fn installed_dir(home: &std::path::Path, dir: &str, name: &str, depends: &str, text: &str) {
    let path = home.join("extensions").join(dir);
    fs::create_dir_all(&path).unwrap_or_else(|e| panic!("mkdir: {e}"));
    fs::write(
        path.join("extension.json"),
        format!(
            r#"{{"name":"{name}","version":"v1.0.0","fiber":"0.1.0","api":1,"depends":{depends}}}"#
        ),
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    fs::write(path.join(".fiber.json"), text).unwrap_or_else(|e| panic!("write: {e}"));
}

#[test]
#[allow(clippy::unwrap_used, reason = "a failure is the test's")]
fn a_floor_in_the_record_lists_with_the_extension() {
    let home = fakes::TempDir::new("fiber-read-floor");
    installed_dir(
        home.path(),
        "example.com-acme-dep",
        "example.com/acme/dep",
        "{}",
        r#"{"name":"example.com/acme/dep","version":"v1.5.0","requested":false,"floor":"v1.5.0","source":{"commit":"abc"}}"#,
    );
    let listing = read(home.path()).unwrap();
    assert_eq!(listing.installed.len(), 1);
    assert_eq!(listing.installed[0].floor.as_deref(), Some("v1.5.0"));
    assert!(!listing.installed[0].requested);
    assert!(listing.damaged.is_empty());
}

#[test]
#[allow(clippy::unwrap_used, reason = "a failure is the test's")]
fn a_bad_floor_lists_the_extension_as_damaged() {
    for floor in ["null", "1", "\"1.2\"", "\"x\""] {
        let home = fakes::TempDir::new("fiber-read-bad-floor");
        installed_dir(
            home.path(),
            "example.com-acme-dep",
            "example.com/acme/dep",
            "{}",
            &format!(
                r#"{{"name":"example.com/acme/dep","version":"v1.5.0","requested":false,"floor":{floor},"source":{{"commit":"abc"}}}}"#
            ),
        );
        let listing = read(home.path()).unwrap();
        assert!(listing.installed.is_empty(), "{floor}");
        assert_eq!(listing.damaged.len(), 1, "{floor}");
        assert_eq!(listing.damaged[0].name, "example.com/acme/dep", "{floor}");
    }
}

#[test]
#[allow(clippy::unwrap_used, reason = "a failure is the test's")]
fn a_floor_does_not_keep_an_orphaned_dependency_installed() {
    let home = fakes::TempDir::new("fiber-remove-floor-orphan");
    let root = "example.com/acme/root";
    let dep = "example.com/acme/dep";
    installed_dir(
        home.path(),
        "example.com-acme-root",
        root,
        &format!(r#"{{"{dep}":"1.0"}}"#),
        &format!(
            r#"{{"name":"{root}","version":"v1.0.0","requested":true,"source":{{"commit":"abc"}}}}"#
        ),
    );
    installed_dir(
        home.path(),
        "example.com-acme-dep",
        dep,
        "{}",
        &format!(
            r#"{{"name":"{dep}","version":"v1.5.0","requested":false,"floor":"v1.5.0","source":{{"commit":"def"}}}}"#
        ),
    );
    let clock = fakes::clock::FakeClock::new();
    let removal = super::removal(home.path(), root, &*clock).unwrap();
    assert_eq!(removal.names, [root, dep]);
    removal.commit().unwrap();
    assert!(
        read(home.path()).unwrap().installed.is_empty(),
        "the floor goes with its directory"
    );
}

/// An installed extension with settings files: `extensions/<dir>/` holding
/// a healthy record, and `config/<dir>.json` in the global and each
/// project layer.
fn installed_with_settings(home: &std::path::Path, name: &str, dir: &str, projects: &[&str]) {
    installed_dir(
        home,
        dir,
        name,
        "{}",
        &format!(
            r#"{{"name":"{name}","version":"v1.0.0","requested":true,"source":{{"commit":"abc"}}}}"#
        ),
    );
    for layer in std::iter::once(home.to_path_buf()).chain(
        projects
            .iter()
            .map(|project| home.join("projects").join(project)),
    ) {
        let file = layer.join("config").join(format!("{dir}.json"));
        fs::create_dir_all(file.parent().unwrap()).unwrap_or_else(|e| panic!("mkdir: {e}"));
        fs::write(&file, "{}").unwrap_or_else(|e| panic!("write: {e}"));
    }
}

#[test]
#[allow(clippy::unwrap_used, reason = "a failure is the test's")]
fn removal_deletes_the_settings_files_in_every_layer() {
    let home = fakes::TempDir::new("fiber-remove-settings");
    let name = "example.com/acme/x";
    let dir = "example.com-acme-x";
    installed_with_settings(home.path(), name, dir, &["p1", "p2"]);
    for layer in ["", "projects/p1", "projects/p2"] {
        let file = home.path().join(layer).join("config/other.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{}").unwrap();
    }
    let clock = fakes::clock::FakeClock::new();
    let removal = super::removal(home.path(), name, &*clock).unwrap();
    assert_eq!(removal.names, [name]);
    removal.commit().unwrap();
    for layer in ["", "projects/p1", "projects/p2"] {
        assert!(
            !home
                .path()
                .join(layer)
                .join(format!("config/{dir}.json"))
                .exists(),
            "{layer}: the settings file is gone"
        );
        assert!(
            home.path().join(layer).join("config/other.json").exists(),
            "{layer}: another extension's settings stay"
        );
    }
    assert!(
        !home.path().join("extensions").join(dir).exists(),
        "the extension directory is gone"
    );
}

#[test]
#[allow(clippy::unwrap_used, reason = "a failure is the test's")]
fn removal_deletes_a_damaged_extensions_settings() {
    let home = fakes::TempDir::new("fiber-remove-damaged-settings");
    let name = "example.com/acme/x";
    let dir = "example.com-acme-x";
    // No record, so the directory reads as damaged under its manifest
    // name, which slugs to the directory.
    let path = home.path().join("extensions").join(dir);
    fs::create_dir_all(&path).unwrap();
    fs::write(
        path.join("extension.json"),
        format!(r#"{{"name":"{name}","version":"v1.0.0","fiber":"0.1.0","api":1}}"#),
    )
    .unwrap();
    for layer in [home.path().to_path_buf(), home.path().join("projects/p1")] {
        let file = layer.join("config").join(format!("{dir}.json"));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{}").unwrap();
    }
    let clock = fakes::clock::FakeClock::new();
    let removal = super::removal(home.path(), name, &*clock).unwrap();
    assert_eq!(removal.names, [name]);
    removal.commit().unwrap();
    assert!(!home.path().join(format!("config/{dir}.json")).exists());
    assert!(
        !home
            .path()
            .join(format!("projects/p1/config/{dir}.json"))
            .exists()
    );
}

#[test]
#[allow(clippy::unwrap_used, reason = "a failure is the test's")]
fn removal_still_lists_the_settings_files() {
    let home = fakes::TempDir::new("fiber-remove-lists-settings");
    let name = "example.com/acme/x";
    let dir = "example.com-acme-x";
    installed_with_settings(home.path(), name, dir, &["p1"]);
    let data_dir = home.path().join("data").join(dir);
    fs::create_dir_all(&data_dir).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let removal = super::removal(home.path(), name, &*clock).unwrap();
    // The prompt names every file it deletes: the data directory and
    // each layer's settings file.
    assert_eq!(
        removal.data,
        vec![
            data_dir,
            home.path().join(format!("config/{dir}.json")),
            home.path().join(format!("projects/p1/config/{dir}.json")),
        ]
    );
}

#[test]
#[allow(clippy::unwrap_used, reason = "a failure is the test's")]
fn a_settings_failure_names_the_file() {
    let home = fakes::TempDir::new("fiber-remove-settings-failure");
    let name = "example.com/acme/x";
    let dir = "example.com-acme-x";
    installed_with_settings(home.path(), name, dir, &[]);
    // A settings path that is a directory is damage `config` names,
    // not a directory `commit` removes.
    let settings = home.path().join(format!("config/{dir}.json"));
    fs::remove_file(&settings).unwrap();
    fs::create_dir_all(settings.join("sub")).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let removal = super::removal(home.path(), name, &*clock).unwrap();
    let err = removal.commit().unwrap_err();
    assert!(
        matches!(err, crate::Error::Config(_)),
        "the settings failure is a config error: {err}"
    );
    assert!(
        err.to_string().contains(&settings.display().to_string()),
        "{err}"
    );
}
