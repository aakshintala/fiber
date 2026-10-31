//! The session's diagnostic writer (`docs/state.md`, "Diagnostic logs"):
//! `host.log` lines append one JSON line each to
//! `logs/session-<id>.log`, creating `logs/` mode 0700 on the first
//! write. A write failure is ignored.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::fs;
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
fn the_first_line_writes_the_session_file_with_the_extension_prefix() {
    let (home, diag) = setup("fiber-diag-first");
    diag.extension_log("fiber.test/notes", "hello");
    assert_eq!(
        fs::read_to_string(log_path(&home)).unwrap(),
        "{\"ts\":1700000000000,\"level\":\"info\",\"process\":\"session\",\
         \"session_id\":\"s-1\",\"code\":\"extension_log\",\
         \"message\":\"fiber.test/notes: hello\"}\n"
    );
}

fn some_sample() -> Option<log::diag::ResourcesSample> {
    Some(log::diag::ResourcesSample {
        peak_kib: 11840,
        current_kib: 11840,
        disk_read_bytes: 4096,
        disk_write_bytes: 8192,
    })
}

fn no_sample() -> Option<log::diag::ResourcesSample> {
    None
}

fn debug_diag(
    home: &fakes::TempDir,
    read: fn() -> Option<log::diag::ResourcesSample>,
) -> std::sync::Arc<log::diag::Diag> {
    let clock = fakes::clock::FakeClock::new();
    let diag = log::diag::Diag::new(
        home.path(),
        log::diag::Process::Session,
        log::diag::Level::Debug,
        clock as std::sync::Arc<dyn contract::clock::Clock>,
    )
    .with_resources(read);
    diag.attach(&SessionId("s-1".into()));
    std::sync::Arc::new(diag)
}

#[test]
fn the_shared_writer_puts_extension_request_and_memory_lines_in_one_file() {
    let home = fakes::TempDir::new("fiber-diag-shared");
    let diag = SessionDiag::shared(debug_diag(&home, some_sample));
    diag.extension_log("fiber.test/notes", "hello");
    diag.resources();
    let text = fs::read_to_string(log_path(&home)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(first["code"], "extension_log");
    assert_eq!(second["code"], "resources");
    assert_eq!(second["data"]["peak_kib"], 11840);
    assert_eq!(second["data"]["current_kib"], 11840);
    assert_eq!(second["data"]["disk_read_bytes"], 4096);
    assert_eq!(second["data"]["disk_write_bytes"], 8192);
    for line in [&first, &second] {
        assert_eq!(line["process"], "session");
        assert_eq!(line["session_id"], "s-1");
    }
}

#[test]
fn a_failed_memory_read_writes_nothing_and_changes_nothing_else() {
    let home = fakes::TempDir::new("fiber-diag-no-peak");
    let diag = SessionDiag::shared(debug_diag(&home, no_sample));
    diag.resources();
    assert!(
        !log_path(&home).exists(),
        "a failed read writes no line and creates no file"
    );
    diag.extension_log("fiber.test/notes", "hello");
    let text = fs::read_to_string(log_path(&home)).unwrap();
    assert_eq!(text.lines().count(), 1, "other lines are unchanged");
}

#[test]
fn an_info_writer_writes_no_memory_line_and_no_debug_file() {
    let home = fakes::TempDir::new("fiber-diag-info-peak");
    let clock = fakes::clock::FakeClock::new();
    let diag = log::diag::Diag::new(
        home.path(),
        log::diag::Process::Session,
        log::diag::Level::Info,
        clock as std::sync::Arc<dyn contract::clock::Clock>,
    );
    diag.attach(&SessionId("s-1".into()));
    let diag = SessionDiag::shared(std::sync::Arc::new(diag));
    diag.resources();
    assert!(
        !log_path(&home).exists(),
        "an info writer creates no file for a memory sample"
    );
}
