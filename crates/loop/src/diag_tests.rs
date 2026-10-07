//! The session's diagnostic writer (`docs/state.md`, "Diagnostic logs"):
//! `host.log` lines append one JSON line each to
//! `logs/session-<id>.log`, creating `logs/` mode 0700 on the first
//! write. A write failure is ignored.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use contract::SessionId;

use super::*;

fn setup(name: &str) -> (fakes::TempDir, SessionDiag) {
    let home = fakes::TempDir::new(name);
    let clock = fakes::clock::FakeClock::new();
    let diag = SessionDiag::new(
        home.path(),
        SessionId("s-1".into()),
        clock as Arc<dyn contract::clock::Clock>,
    );
    (home, diag)
}

fn log_path(home: &fakes::TempDir) -> PathBuf {
    home.path().join("logs").join("session-s-1.log")
}

#[test]
fn the_first_line_creates_logs_mode_0700_and_the_session_file() {
    let (home, diag) = setup("fiber-diag-first");
    assert!(!home.path().join("logs").exists());
    diag.extension_log("fiber.test/notes", "hello");
    let logs = home.path().join("logs");
    let mode = fs::metadata(&logs).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    assert_eq!(
        fs::read_to_string(log_path(&home)).unwrap(),
        "{\"ts\":1700000000000,\"level\":\"info\",\"process\":\"session\",\
         \"session_id\":\"s-1\",\"code\":\"extension_log\",\
         \"message\":\"fiber.test/notes: hello\"}\n"
    );
}

#[test]
fn two_lines_append_in_order() {
    let (home, diag) = setup("fiber-diag-two");
    diag.extension_log("fiber.test/notes", "first");
    diag.extension_log("fiber.test/notes", "second");
    let text = fs::read_to_string(log_path(&home)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with("\"message\":\"fiber.test/notes: first\"}"));
    assert!(lines[1].ends_with("\"message\":\"fiber.test/notes: second\"}"));
}

#[test]
fn a_newline_and_a_quote_stay_one_line_and_round_trip() {
    let (home, diag) = setup("fiber-diag-escape");
    diag.extension_log("fiber.test/notes", "a\nb\"c");
    let text = fs::read_to_string(log_path(&home)).unwrap();
    assert_eq!(text.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(value["message"], "fiber.test/notes: a\nb\"c");
}

#[test]
fn an_unwritable_logs_is_ignored_without_panic() {
    let (home, diag) = setup("fiber-diag-unwritable");
    fs::write(home.path().join("logs"), b"not a directory").unwrap();
    diag.extension_log("fiber.test/notes", "hello");
}
