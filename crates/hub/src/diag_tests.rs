//! Tests for the hub's diagnostic log (`docs/state.md`, "Diagnostic logs"
//! and "Bounds"): the line shape, rotation past 10 MiB, and pruning.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs::{self, File};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, SystemTime};

use contract::SessionId;

use super::*;

/// `FakeClock::new` reads the process clock once for its monotonic origin;
/// its wall time is fixed: 2023-11-14T22:13:20Z.
const WALL: u64 = 1_700_000_000;
const DAY: u64 = 24 * 60 * 60;

fn wall() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(WALL)
}

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hg");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self { dir, held }
    }

    fn diag(&self) -> Diag {
        Diag::open(&self.dir, fakes::clock::FakeClock::new())
    }

    fn log(&self) -> PathBuf {
        self.dir.join("logs").join("hub.log")
    }

    fn text(&self) -> String {
        fs::read_to_string(self.log()).unwrap()
    }
}

#[test]
fn a_line_carries_ts_level_process_code_and_message_in_order() {
    let temp = Temp::new();
    let diag = temp.diag();
    diag.info("hub_started", "The hub started.");
    assert_eq!(
        temp.text(),
        "{\"ts\":1700000000000,\"level\":\"info\",\"process\":\"hub\",\
         \"code\":\"hub_started\",\"message\":\"The hub started.\"}\n"
    );
}

#[test]
fn a_line_naming_a_session_carries_session_id_after_process() {
    let temp = Temp::new();
    let diag = temp.diag();
    diag.info_session(
        &SessionId("s_0123456789abcdef".into()),
        "session_started",
        "Session started for local.",
    );
    assert_eq!(
        temp.text(),
        "{\"ts\":1700000000000,\"level\":\"info\",\"process\":\"hub\",\
         \"session_id\":\"s_0123456789abcdef\",\"code\":\"session_started\",\
         \"message\":\"Session started for local.\"}\n"
    );
}

#[test]
fn a_failure_is_a_warn_line_with_the_rejection_code() {
    let temp = Temp::new();
    let diag = temp.diag();
    diag.warn_session(
        &SessionId("s_0123456789abcdef".into()),
        "io_failed",
        "Session s_0123456789abcdef could not start: refused.",
    );
    assert_eq!(
        temp.text(),
        "{\"ts\":1700000000000,\"level\":\"warn\",\"process\":\"hub\",\
         \"session_id\":\"s_0123456789abcdef\",\"code\":\"io_failed\",\"message\":\"Session s_0123456789abcdef could not start: refused.\"}\n"
    );
}

#[test]
fn a_log_of_exactly_10_mib_is_not_rotated() {
    let temp = Temp::new();
    fs::create_dir_all(temp.dir.join("logs")).unwrap();
    fs::write(temp.log(), vec![b'x'; 10 * 1024 * 1024]).unwrap();
    temp.diag().info("hub_started", "The hub started.");
    assert!(!temp.dir.join("logs").join("hub.log.1").exists());
    assert!(fs::metadata(temp.log()).unwrap().len() > 10 * 1024 * 1024);
}

#[test]
fn a_log_over_10_mib_is_renamed_before_the_next_write() {
    let temp = Temp::new();
    fs::create_dir_all(temp.dir.join("logs")).unwrap();
    let old = vec![b'x'; 10 * 1024 * 1024 + 1];
    fs::write(temp.log(), &old).unwrap();
    temp.diag().info("hub_started", "The hub started.");
    let previous = temp.dir.join("logs").join("hub.log.1");
    assert_eq!(fs::read(&previous).unwrap(), old);
    assert!(temp.text().ends_with("The hub started.\"}\n"));
    assert!(fs::metadata(temp.log()).unwrap().len() < 1024);
}

#[test]
fn prune_deletes_files_older_than_30_days_and_keeps_the_boundary() {
    let temp = Temp::new();
    let logs = temp.dir.join("logs");
    fs::create_dir_all(&logs).unwrap();
    let old = logs.join("old.log");
    let edge = logs.join("edge.log");
    let fresh = logs.join("fresh.log");
    fs::write(&old, b"old").unwrap();
    fs::write(&edge, b"edge").unwrap();
    fs::write(&fresh, b"fresh").unwrap();
    File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(wall() - Duration::from_secs(30 * DAY + 1))
        .unwrap();
    File::options()
        .write(true)
        .open(&edge)
        .unwrap()
        .set_modified(wall() - Duration::from_secs(30 * DAY))
        .unwrap();
    File::options()
        .write(true)
        .open(&fresh)
        .unwrap()
        .set_modified(wall() - Duration::from_secs(DAY))
        .unwrap();
    temp.diag();
    assert!(!old.exists());
    assert!(edge.exists());
    assert!(fresh.exists());
}

#[test]
fn concurrent_writes_past_the_limit_lose_no_records() {
    const WRITERS: usize = 8;
    const EACH: usize = 50;
    let temp = Temp::new();
    // Fill `hub.log` to just under the limit with countable lines, so the
    // run below rotates exactly once.
    fs::create_dir_all(temp.dir.join("logs")).unwrap();
    let line = "{\"ts\":1700000000000,\"level\":\"info\",\"process\":\"hub\",\
         \"code\":\"hub_started\",\"message\":\"The hub started.\"}\n";
    let prefill = (10 * 1024 * 1024 - 4096) / line.len();
    fs::write(temp.log(), line.repeat(prefill)).unwrap();
    let diag = Arc::new(temp.diag());
    let barrier = Arc::new(Barrier::new(WRITERS));
    let mut threads = Vec::new();
    for _ in 0..WRITERS {
        let (diag, barrier) = (Arc::clone(&diag), Arc::clone(&barrier));
        threads.push(
            thread::Builder::new()
                .name("hub-test-diag".to_owned())
                .spawn(move || {
                    barrier.wait();
                    for _ in 0..EACH {
                        diag.info("hub_started", "The hub started.");
                    }
                })
                .unwrap(),
        );
    }
    for thread in threads {
        thread.join().unwrap();
    }
    // One rotation: every record is in one of the two files, and parses.
    let previous = temp.dir.join("logs").join("hub.log.1");
    assert!(previous.exists());
    let mut lines = 0;
    for file in [temp.log(), previous] {
        let text = fs::read_to_string(&file).unwrap();
        for line in text.lines() {
            serde_json::from_str::<serde_json::Value>(line).expect("a log line parses");
            lines += 1;
        }
    }
    assert_eq!(lines, prefill + WRITERS * EACH);
}

#[test]
fn prune_keeps_the_newest_100_files_by_mtime() {
    let temp = Temp::new();
    let crashes = temp.dir.join("crashes");
    fs::create_dir_all(&crashes).unwrap();
    for i in 0..101 {
        let path = crashes.join(format!("{i:03}.txt"));
        fs::write(&path, b"x").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_000_000 + i))
            .unwrap();
    }
    temp.diag();
    assert!(!crashes.join("000.txt").exists());
    for i in 1..101 {
        assert!(crashes.join(format!("{i:03}.txt")).exists());
    }
}

#[test]
fn prune_keeps_100_files_and_leaves_subdirectories_alone() {
    let temp = Temp::new();
    let logs = temp.dir.join("logs");
    fs::create_dir_all(logs.join("sub")).unwrap();
    for i in 0..100 {
        let path = logs.join(format!("{i:03}.log"));
        fs::write(&path, b"x").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_000_000 + i))
            .unwrap();
    }
    temp.diag();
    assert_eq!(fs::read_dir(&logs).unwrap().count(), 101);
}
