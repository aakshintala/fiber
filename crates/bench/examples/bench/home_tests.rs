use std::fs;

use fakes::TempDir;
use serde_json::json;

use super::{config, read_logs};

#[test]
fn config_without_diagnostics_is_the_default_install() {
    assert_eq!(
        config(false),
        json!({
            "model": "fake/m",
            "session": {"idle_exit_ms": 3600000},
            "hub": {"idle_exit_ms": 1000}
        })
    );
}

#[test]
fn config_with_debug_adds_the_debug_level() {
    assert_eq!(
        config(true),
        json!({
            "model": "fake/m",
            "session": {"idle_exit_ms": 3600000},
            "hub": {"idle_exit_ms": 1000},
            "diagnostics": {"level": "debug"}
        })
    );
}

#[test]
fn read_logs_returns_each_log_file_in_file_name_order() {
    let dir = TempDir::new("logs");
    fs::write(dir.path().join("session-s_a.log"), "session\n").unwrap();
    fs::write(dir.path().join("hub.log"), "hub\n").unwrap();
    fs::write(dir.path().join("notes.txt"), "not a log\n").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    assert_eq!(
        read_logs(dir.path()),
        vec![Ok("hub\n".to_owned()), Ok("session\n".to_owned())]
    );
}

#[test]
fn read_logs_of_a_missing_directory_is_empty() {
    let dir = TempDir::new("logs");
    fs::write(dir.path().join("a.log"), "present\n").unwrap();
    assert_eq!(read_logs(dir.path()), vec![Ok("present\n".to_owned())]);
    assert!(read_logs(&dir.path().join("absent")).is_empty());
}

#[test]
fn read_logs_names_a_log_path_it_cannot_read() {
    let dir = TempDir::new("logs");
    fs::create_dir(dir.path().join("bad.log")).unwrap();
    let logs = read_logs(dir.path());
    let [Err(err)] = logs.as_slice() else {
        panic!("one unreadable log, got {logs:?}");
    };
    assert!(err.contains("bad.log"), "{err}");
}
