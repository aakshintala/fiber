//! Reading a session: the torn tail a reader skips, and watchers
//! (`docs/events.md`, "Writing"; `docs/architecture.md`, "Streaming").

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;

use common::*;
use contract::Envelope;
use log::{Log, Watcher, read};

fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|l| l.kind.as_str()).collect()
}

fn recv_all(watcher: &mut Watcher, n: usize) -> Vec<Envelope> {
    (0..n).map(|_| watcher.recv().unwrap().unwrap()).collect()
}

#[test]
fn a_reader_gets_every_durable_line_and_stops_at_the_last_complete_one() {
    let tmp = TestDir::new("read");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    let written = log.append(&session_started(), None, None).unwrap();
    log.append(&delta("x"), None, None).unwrap();
    let dir = tmp.session(&id("s_1"));
    let path = dir.join("events.jsonl");
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(br#"{"kind":"step_started","session_id":"s_1","#)
        .unwrap();
    // The writer still holds the session: a reader needs no lock.
    assert_eq!(read(&dir).unwrap(), [written]);
}

#[test]
fn a_reader_refuses_a_complete_line_that_does_not_parse_and_names_it() {
    let tmp = TestDir::new("read-bad");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    log.append(&session_started(), None, None).unwrap();
    let dir = tmp.session(&id("s_1"));
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    file.write_all(b"not json\n").unwrap();
    let err = read(&dir).unwrap_err();
    assert!(err.to_string().contains("line 2"), "{err}");
    assert_eq!(err.code(), None);
}

#[test]
fn reading_a_missing_session_is_not_found() {
    let tmp = TestDir::new("read-missing");
    let err = read(&tmp.session(&id("s_x"))).unwrap_err();
    assert_eq!(err.code(), Some(contract::ErrorCode::SessionNotFound));
}

#[test]
fn a_watcher_receives_what_is_written_after_it_subscribes() {
    let tmp = TestDir::new("watch");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    log.append(&session_started(), None, None).unwrap();
    let mut watcher = log.watch();
    let step = log.append(&empty("step_started"), None, None).unwrap();
    let delta = log.append(&delta("Hel"), None, None).unwrap();
    assert_eq!(recv_all(&mut watcher, 2), [step, delta]);
}

#[test]
fn a_watcher_on_another_thread_wakes_for_each_line_and_ends_with_the_log() {
    let tmp = TestDir::new("watch-thread");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    let mut watcher = log.watch();
    let reader = thread::spawn(move || {
        let mut got = Vec::new();
        while let Some(line) = watcher.recv().unwrap() {
            got.push(line);
        }
        got
    });
    let written = vec![
        log.append(&session_started(), None, None).unwrap(),
        log.append(&delta("a"), None, None).unwrap(),
        log.append(&empty("step_started"), None, None).unwrap(),
    ];
    drop(log);
    assert_eq!(reader.join().unwrap(), written);
}

#[test]
fn a_watcher_that_falls_behind_rereads_durable_lines_from_the_log() {
    let tmp = TestDir::new("watch-lag");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    let mut watcher = log.watch();
    // Far past any bounded queue, ending on a durable line, then an
    // ephemeral one that a lagging watcher may lose.
    let n = 5000;
    let mut durable = Vec::new();
    for _ in 0..n {
        durable.push(log.append(&empty("step_started"), None, None).unwrap());
        log.append(&delta("x"), None, None).unwrap();
    }
    let mut got = Vec::new();
    while got.iter().filter(|l: &&Envelope| l.is_durable()).count() < n {
        got.push(watcher.recv().unwrap().unwrap());
    }
    let got_durable: Vec<_> = got.into_iter().filter(Envelope::is_durable).collect();
    assert_eq!(got_durable, durable);
    // Lines written after catching up arrive as they are written.
    let after = log.append(&delta("live"), None, None).unwrap();
    assert_eq!(watcher.recv().unwrap(), Some(after));
}

#[test]
fn a_watcher_ends_when_its_log_is_dropped() {
    let tmp = TestDir::new("watch-end");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    let mut watcher = log.watch();
    let line = log.append(&session_started(), None, None).unwrap();
    drop(log);
    assert_eq!(watcher.recv().unwrap(), Some(line));
    assert_eq!(watcher.recv().unwrap(), None);
}

#[test]
fn a_dropped_watcher_does_not_stop_the_log() {
    let tmp = TestDir::new("watch-drop");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    drop(log.watch());
    let mut kept = log.watch();
    let line = log.append(&session_started(), None, None).unwrap();
    assert_eq!(kept.recv().unwrap(), Some(line));
    assert_eq!(
        kinds(&read(&tmp.session(&id("s_1"))).unwrap()),
        ["session_started"]
    );
}

/// Run as a child by `a_writer_in_another_process_holds_the_session`: holds
/// the session named by `LOG_TEST_SESSIONS`, says so, and lets go when its
/// stdin closes. Run on its own it does nothing.
#[test]
#[allow(clippy::print_stdout, reason = "the parent test reads this line")]
fn child_holding_a_session() {
    let Ok(sessions) = std::env::var("LOG_TEST_SESSIONS") else {
        return;
    };
    let log = Log::open(std::path::Path::new(&sessions), id("s_1")).unwrap();
    println!("holding");
    std::io::stdout().flush().unwrap();
    let mut rest = String::new();
    std::io::stdin().read_line(&mut rest).unwrap();
    drop(log);
}

#[test]
fn a_writer_in_another_process_holds_the_session() {
    let tmp = TestDir::new("lock-child");
    let log = Log::create(tmp.path(), id("s_1")).unwrap();
    log.append(&session_started(), None, None).unwrap();
    drop(log);

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_holding_a_session", "--nocapture"])
        .env("LOG_TEST_SESSIONS", tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while line.trim() != "holding" {
        line.clear();
        assert_ne!(out.read_line(&mut line).unwrap(), 0, "the child exited");
    }

    let Err(err) = Log::open(tmp.path(), id("s_1")) else {
        panic!("opened a session another process holds");
    };
    assert!(
        err.to_string().contains(&format!("process {}", child.id())),
        "{err}"
    );

    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    assert!(Log::open(tmp.path(), id("s_1")).is_ok());
}
