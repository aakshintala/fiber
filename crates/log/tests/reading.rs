//! Reading a session: the torn tail a reader skips, and watchers
//! (`docs/events.md`, "Writing"; `docs/architecture.md`, "Streaming").

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use common::*;
use contract::Envelope;
use log::{Log, Watcher, lines, read};

fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|l| l.kind.as_str()).collect()
}

/// How long a test waits for a watcher to receive a line.
const DEADLINE: Duration = Duration::from_secs(10);

/// Runs `watcher` on its own thread and passes on what it receives, so a
/// test can wait for each line with a deadline. `None` is the end.
fn relay(mut watcher: Watcher) -> Receiver<Option<Envelope>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        loop {
            let got = watcher.recv().unwrap();
            let end = got.is_none();
            if tx.send(got).is_err() || end {
                break;
            }
        }
    });
    rx
}

/// The watcher's next line, or the end.
fn next(rx: &Receiver<Option<Envelope>>) -> Option<Envelope> {
    rx.recv_timeout(DEADLINE)
        .expect("the watcher to receive a line before the deadline")
}

#[test]
fn a_reader_gets_every_durable_line_and_stops_at_the_last_complete_one() {
    let tmp = TestDir::new("read");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
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
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    log.append(&session_started(), None, None).unwrap();
    let dir = tmp.session(&id("s_1"));
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    file.write_all(b"not json\n").unwrap();
    let err = read(&dir).unwrap_err();
    assert!(err.to_string().contains("line 2"), "{err}");
    assert_eq!(err.code(), contract::ErrorCode::LogCorrupt);
}

#[test]
fn reading_a_missing_session_is_not_found() {
    let tmp = TestDir::new("read-missing");
    let err = read(&tmp.session(&id("s_x"))).unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::SessionNotFound);
}

/// Appends `line` to the session's log as raw bytes, behind the writer's back.
fn append_raw(dir: &std::path::Path, line: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    file.write_all(line).unwrap();
}

#[test]
fn lines_yield_every_complete_line_in_order_and_skip_a_torn_tail() {
    let tmp = TestDir::new("lines");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let written = [
        log.append(&session_started(), None, None).unwrap(),
        log.append(&empty("step_started"), None, None).unwrap(),
        log.append(&empty("step_started"), None, None).unwrap(),
    ];
    let dir = tmp.session(&id("s_1"));
    append_raw(&dir, br#"{"kind":"step_started","session_id":"s_1","#);
    let got: Vec<Envelope> = lines(&dir).unwrap().map(Result::unwrap).collect();
    assert_eq!(got, written);
}

#[test]
fn lines_read_a_line_longer_than_64_kib_whole() {
    let tmp = TestDir::new("lines-long");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let long = log
        .append(
            &event(
                "session_named",
                serde_json::json!({"name": "n".repeat(100 * 1024), "by": "person"}),
            ),
            None,
            None,
        )
        .unwrap();
    let after = log.append(&empty("step_started"), None, None).unwrap();
    let got: Vec<Envelope> = lines(&tmp.session(&id("s_1")))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(got, [long, after]);
}

#[test]
fn lines_end_after_an_unparseable_line_that_they_name() {
    let tmp = TestDir::new("lines-bad");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let first = log.append(&session_started(), None, None).unwrap();
    let dir = tmp.session(&id("s_1"));
    append_raw(&dir, b"not json\n");
    // A good line after the bad one is never reached.
    let whole = fs::read(dir.join("events.jsonl")).unwrap();
    let first_line = whole
        .split_inclusive(|b| *b == b'\n')
        .next()
        .unwrap()
        .to_vec();
    append_raw(&dir, &first_line);
    let mut it = lines(&dir).unwrap();
    assert_eq!(it.next().unwrap().unwrap(), first);
    let err = it.next().unwrap().unwrap_err();
    assert!(err.to_string().contains("line 2"), "{err}");
    assert_eq!(err.code(), contract::ErrorCode::LogCorrupt);
    assert!(it.next().is_none(), "the lines end after the first error");
}

#[test]
fn lines_of_a_missing_session_are_not_found() {
    let tmp = TestDir::new("lines-missing");
    let Err(err) = lines(&tmp.session(&id("s_x"))) else {
        panic!("read lines of a session that does not exist");
    };
    assert_eq!(err.code(), contract::ErrorCode::SessionNotFound);
}

#[test]
fn a_watcher_receives_what_is_written_after_it_subscribes() {
    let tmp = TestDir::new("watch");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    log.append(&session_started(), None, None).unwrap();
    let rx = relay(log.watch());
    let step = log.append(&empty("step_started"), None, None).unwrap();
    let delta = log.append(&delta("Hel"), None, None).unwrap();
    assert_eq!(next(&rx), Some(step));
    assert_eq!(next(&rx), Some(delta));
}

#[test]
fn a_watcher_ends_with_its_log_once_it_has_every_line() {
    let tmp = TestDir::new("watch-end");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let rx = relay(log.watch());
    let written = [
        log.append(&session_started(), None, None).unwrap(),
        log.append(&delta("a"), None, None).unwrap(),
    ];
    drop(log);
    assert_eq!(next(&rx), Some(written[0].clone()));
    assert_eq!(next(&rx), Some(written[1].clone()));
    assert_eq!(next(&rx), None);
}

#[test]
fn a_watcher_that_falls_behind_rereads_durable_lines_from_the_log() {
    let tmp = TestDir::new("watch-lag");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let watcher = log.watch();
    // Past any bounded queue while nobody receives, ending on a durable line.
    let durable: Vec<Envelope> = (0..600)
        .map(|_| {
            log.append(&delta("x"), None, None).unwrap();
            log.append(&empty("step_started"), None, None).unwrap()
        })
        .collect();
    let rx = relay(watcher);
    let mut got = Vec::new();
    let mut ephemeral = 0;
    while got.len() < durable.len() {
        let line = next(&rx).unwrap();
        if line.is_durable() {
            got.push(line);
        } else {
            ephemeral += 1;
        }
    }
    assert_eq!(got, durable);
    // The queue is bounded: the ephemeral lines it had no room for are gone.
    assert!(ephemeral < durable.len(), "{ephemeral} ephemeral lines");
    // Once caught up, lines arrive as they are written.
    let live = log.append(&delta("live"), None, None).unwrap();
    assert_eq!(next(&rx), Some(live));
}

#[test]
fn recv_timeout_returns_an_available_line_without_waiting() {
    let tmp = TestDir::new("recv-timeout-ready");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let mut watcher = log.watch();
    let written = log.append(&session_started(), None, None).unwrap();
    let got = watcher
        .recv_timeout(Duration::ZERO)
        .expect("an available line needs no wait")
        .unwrap()
        .expect("the line written before the call");
    assert_eq!(got, written);
}

#[test]
fn recv_timeout_times_out_without_losing_the_next_line() {
    let tmp = TestDir::new("recv-timeout-empty");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let mut watcher = log.watch();
    assert!(
        watcher.recv_timeout(Duration::ZERO).is_none(),
        "nothing written yet"
    );
    let written = log.append(&session_started(), None, None).unwrap();
    let got = watcher
        .recv_timeout(DEADLINE)
        .expect("a line written after the timeout")
        .unwrap()
        .expect("the line written after the timeout");
    assert_eq!(got, written);
}

#[test]
fn recv_timeout_returns_a_line_written_while_it_waits() {
    let tmp = TestDir::new("recv-timeout-wait");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let mut watcher = log.watch();
    thread::scope(|s| {
        let waiting = s.spawn(|| {
            watcher
                .recv_timeout(DEADLINE)
                .expect("a line written while waiting")
        });
        let written = log.append(&session_started(), None, None).unwrap();
        let got = waiting.join().unwrap().unwrap().expect("the waited line");
        assert_eq!(got, written);
    });
}

#[test]
fn recv_timeout_ends_with_its_log_once_it_has_every_line() {
    let tmp = TestDir::new("recv-timeout-end");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let mut watcher = log.watch();
    let written = [
        log.append(&session_started(), None, None).unwrap(),
        log.append(&delta("a"), None, None).unwrap(),
    ];
    drop(log);
    for line in written {
        let got = watcher
            .recv_timeout(DEADLINE)
            .expect("a line written before the end")
            .unwrap()
            .expect("a line written before the end");
        assert_eq!(got, line);
    }
    let end = watcher
        .recv_timeout(DEADLINE)
        .expect("the end after every line")
        .unwrap();
    assert!(end.is_none(), "the end after every line");
}

#[test]
fn a_watcher_that_falls_behind_catches_up_through_recv_timeout() {
    let tmp = TestDir::new("recv-timeout-lag");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let mut watcher = log.watch();
    // Past any bounded queue while nobody receives, ending on a durable line.
    let durable: Vec<Envelope> = (0..600)
        .map(|_| {
            log.append(&delta("x"), None, None).unwrap();
            log.append(&empty("step_started"), None, None).unwrap()
        })
        .collect();
    // `durable` moves into the thread below; the copy stays for the
    // assertion after it.
    let queued = durable.clone();
    // Calling code that blocks is a wait too (`docs/testing.md`, "Waits
    // and timeouts"): the catch-up loop runs on a thread and hands back
    // what it read, under one deadline for the whole catch-up.
    let (mut watcher, got, ephemeral) = fakes::within("the catch-up lines", DEADLINE, move || {
        let mut got = Vec::new();
        let mut ephemeral = 0;
        while got.len() < queued.len() {
            let line = watcher
                .recv_timeout(DEADLINE)
                .expect("a catch-up line before the deadline")
                .unwrap()
                .expect("a catch-up line before the end");
            if line.is_durable() {
                got.push(line);
            } else {
                ephemeral += 1;
            }
        }
        (watcher, got, ephemeral)
    });
    assert_eq!(got, durable);
    // The queue is bounded: the ephemeral lines it had no room for are gone.
    assert!(ephemeral < durable.len(), "{ephemeral} ephemeral lines");
    // Once caught up, lines arrive as they are written.
    let live = log.append(&delta("live"), None, None).unwrap();
    let got = fakes::within("the live line", DEADLINE, move || {
        watcher
            .recv_timeout(DEADLINE)
            .expect("a live line before the deadline")
            .unwrap()
            .expect("a live line after the catch-up")
    });
    assert_eq!(got, live);
}

#[test]
fn a_dropped_watcher_does_not_stop_the_log() {
    let tmp = TestDir::new("watch-drop");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    drop(log.watch());
    let rx = relay(log.watch());
    let line = log.append(&session_started(), None, None).unwrap();
    assert_eq!(next(&rx), Some(line));
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
    let log = Log::open(
        std::path::Path::new(&sessions),
        id("s_1"),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    println!("holding");
    std::io::stdout().flush().unwrap();
    let mut rest = String::new();
    std::io::stdin().read_line(&mut rest).unwrap();
    drop(log);
}

#[test]
fn a_writer_in_another_process_holds_the_session() {
    let tmp = TestDir::new("lock-child");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    log.append(&session_started(), None, None).unwrap();
    drop(log);

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_holding_a_session", "--nocapture"])
        .env("LOG_TEST_SESSIONS", tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let stdout = child.stdout.take().unwrap();
    // Calling code that blocks is a wait too (`docs/testing.md`, "Waits
    // and timeouts"): the read-until-`holding` loop runs on a thread and
    // sends its reader back, received with a deadline naming the wait.
    let (done, holding) = mpsc::channel();
    thread::spawn(move || {
        let mut out = BufReader::new(stdout);
        let mut line = String::new();
        while line.trim() != "holding" {
            line.clear();
            let read = out
                .read_line(&mut line)
                .expect("the child to print its holding line");
            assert_ne!(read, 0, "the child exited before its holding line");
        }
        done.send(out).unwrap_or(());
    });
    let _out = match holding.recv_timeout(DEADLINE) {
        Ok(out) => out,
        Err(_) => {
            match fakes::kill_pid(pid, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let (done, exited) = mpsc::channel();
            thread::spawn(move || {
                done.send(child.wait()).unwrap_or(());
            });
            let reaped = exited.recv_timeout(DEADLINE).is_ok();
            panic!("waited {DEADLINE:?} for the child's holding line (reaped: {reaped})");
        }
    };

    let Err(err) = Log::open(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()) else {
        panic!("opened a session another process holds");
    };
    assert!(err.to_string().contains(&format!("process {pid}")), "{err}");

    drop(child.stdin.take());
    let (done, exited) = mpsc::channel();
    thread::spawn(move || {
        done.send(child.wait()).unwrap_or(());
    });
    let status = match exited.recv_timeout(DEADLINE) {
        Ok(status) => status,
        Err(_) => {
            match fakes::kill_pid(pid, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let reaped = exited.recv_timeout(DEADLINE).is_ok();
            panic!("waited {DEADLINE:?} for the child to exit (reaped: {reaped})");
        }
    };
    assert!(status.unwrap().success());
    assert!(Log::open(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).is_ok());
}

/// A log of `count` durable lines and what each append returned.
fn steps(name: &str, count: usize) -> (TestDir, Log, Vec<Envelope>) {
    let tmp = TestDir::new(name);
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let written = (0..count)
        .map(|_| log.append(&empty("step_started"), None, None).unwrap())
        .collect();
    (tmp, log, written)
}

#[test]
fn a_range_returns_the_lines_at_its_positions() {
    let (_tmp, log, written) = steps("range", 10);
    assert_eq!(log.count(), 10);
    assert_eq!(log.range(3, 4).unwrap(), written[3..7]);
    assert_eq!(log.range(0, 10).unwrap(), written);
    // The log ends first.
    assert_eq!(log.range(7, 10).unwrap(), written[7..]);
    assert_eq!(log.range(9, 1).unwrap(), written[9..]);
    assert_eq!(log.range(9, 2).unwrap(), written[9..]);
    assert!(log.range(10, 1).unwrap().is_empty());
    assert!(log.range(11, 1).unwrap().is_empty());
    assert!(log.range(3, 0).unwrap().is_empty());
}

#[test]
fn a_range_of_an_empty_log_is_empty() {
    let (_tmp, log, _) = steps("range-empty", 0);
    assert_eq!(log.count(), 0);
    assert!(log.range(0, 5).unwrap().is_empty());
    // An ephemeral line takes no position.
    log.append(&delta("x"), None, None).unwrap();
    assert_eq!(log.count(), 0);
    assert!(log.range(0, 5).unwrap().is_empty());
}

#[test]
fn a_range_reads_lines_longer_than_64_kib_before_and_inside_it() {
    let tmp = TestDir::new("range-long");
    let log = Log::create(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    let long = || {
        event(
            "session_named",
            serde_json::json!({"name": "n".repeat(100 * 1024), "by": "person"}),
        )
    };
    let written = [
        log.append(&long(), None, None).unwrap(),
        log.append(&empty("step_started"), None, None).unwrap(),
        log.append(&long(), None, None).unwrap(),
        log.append(&empty("step_started"), None, None).unwrap(),
    ];
    assert_eq!(log.range(1, 3).unwrap(), written[1..]);
    assert_eq!(log.range(2, 1).unwrap(), written[2..3]);
}

#[test]
fn a_range_holds_lines_appended_after_open() {
    let (tmp, log, mut written) = steps("range-reopen", 3);
    drop(log);
    let log = Log::open(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(log.count(), 3);
    assert_eq!(log.range(0, 10).unwrap(), written);
    for _ in 0..2 {
        written.push(log.append(&empty("step_started"), None, None).unwrap());
    }
    assert_eq!(log.count(), 5);
    assert_eq!(log.range(3, 2).unwrap(), written[3..]);
    assert_eq!(log.range(0, 10).unwrap(), written);
}

#[test]
fn a_range_after_a_torn_tail_ends_before_it_and_appends_carry_on() {
    let (tmp, log, mut written) = steps("range-torn", 2);
    drop(log);
    let dir = tmp.session(&id("s_1"));
    append_raw(&dir, br#"{"kind":"step_started","session_id":"s_1","#);
    let log = Log::open(tmp.path(), id("s_1"), fakes::clock::FakeClock::new()).unwrap();
    assert_eq!(log.range(0, 10).unwrap(), written);
    written.push(log.append(&empty("step_started"), None, None).unwrap());
    assert_eq!(log.range(2, 1).unwrap(), written[2..]);
    assert_eq!(log.range(0, 10).unwrap(), written);
}

#[test]
fn a_range_parses_only_its_own_lines() {
    let (tmp, log, written) = steps("range-bad", 6);
    corrupt(&tmp.session(&id("s_1")), 1);
    // A bad line before the window is never parsed.
    assert_eq!(log.range(2, 4).unwrap(), written[2..]);
    // One inside it is an error naming it.
    let err = log.range(0, 3).unwrap_err();
    assert!(err.to_string().contains("line 2"), "{err}");
    assert_eq!(err.code(), contract::ErrorCode::LogCorrupt);
    let err = log.range(1, 1).unwrap_err();
    assert!(err.to_string().contains("line 2"), "{err}");
}

/// Receives from `rx` until `count` durable lines have arrived, and returns
/// them.
fn durable(rx: &Receiver<Option<Envelope>>, count: usize) -> Vec<Envelope> {
    let mut got = Vec::new();
    while got.len() < count {
        let line = next(rx).unwrap();
        if line.is_durable() {
            got.push(line);
        }
    }
    got
}

#[test]
fn a_watcher_that_falls_behind_by_more_than_two_pages_gets_every_line_once() {
    let (_tmp, log, _) = steps("watch-pages", 3);
    let watcher = log.watch();
    let written: Vec<Envelope> = (0..2 * CAPACITY + 50)
        .map(|_| {
            log.append(&delta("x"), None, None).unwrap();
            log.append(&empty("step_started"), None, None).unwrap()
        })
        .collect();
    let rx = relay(watcher);
    assert_eq!(durable(&rx, written.len()), written);
    // Once caught up, lines arrive as they are written, and none came twice.
    let live = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(durable(&rx, 1), [live]);
}

#[test]
fn a_catch_up_page_ending_at_the_end_of_the_log_carries_on_live() {
    let (_tmp, log, _) = steps("watch-page-end", 0);
    let watcher = log.watch();
    // The queue holds the first `CAPACITY` lines; the catch-up reads the
    // rest as exactly one full page, and finds nothing after it.
    let written: Vec<Envelope> = (0..2 * CAPACITY)
        .map(|_| log.append(&empty("step_started"), None, None).unwrap())
        .collect();
    let rx = relay(watcher);
    assert_eq!(durable(&rx, written.len()), written);
    let live = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(next(&rx), Some(live));
}

#[test]
fn a_catch_up_never_parses_a_line_before_the_watcher_subscribed() {
    let (tmp, log, _) = steps("watch-bad-before", 3);
    let watcher = log.watch();
    corrupt(&tmp.session(&id("s_1")), 1);
    let written: Vec<Envelope> = (0..CAPACITY + 100)
        .map(|_| log.append(&empty("step_started"), None, None).unwrap())
        .collect();
    let rx = relay(watcher);
    assert_eq!(durable(&rx, written.len()), written);
}

#[test]
fn a_watcher_behind_by_more_than_a_queue_catches_up_through_pages_cut_by_bytes() {
    let (_tmp, log, _) = steps("watch-pages-bytes", 0);
    let watcher = log.watch();
    // The queue holds the first `CAPACITY` lines and drops the rest. The
    // catch-up reads the 300 KiB lines after them, about three to a page.
    let mut written: Vec<Envelope> = (0..CAPACITY)
        .map(|_| log.append(&empty("step_started"), None, None).unwrap())
        .collect();
    let reply = "x".repeat(300 * 1024);
    for _ in 0..10 {
        let text = event("text_completed", serde_json::json!({"text": reply}));
        written.push(log.append(&text, None, None).unwrap());
        written.push(log.append(&empty("step_started"), None, None).unwrap());
    }
    let rx = relay(watcher);
    assert_eq!(durable(&rx, written.len()), written);
    let live = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(next(&rx), Some(live));
}

#[test]
fn a_watch_all_watcher_that_falls_behind_catches_up_past_its_end_bound() {
    let (_tmp, log, written) = steps("watch-all-lag", 2 * CAPACITY);
    let watcher = log.watch_all();
    // Nobody drains while the flood is appended, so the queue holds its
    // first `CAPACITY` lines and drops the rest. The backlog stops at the
    // subscribe-time line count; the catch-up reads past it, to the table's
    // current end.
    let flood: Vec<Envelope> = (0..CAPACITY + 500)
        .map(|_| log.append(&empty("step_started"), None, None).unwrap())
        .collect();
    let rx = relay(watcher);
    let expected: Vec<Envelope> = written.into_iter().chain(flood).collect();
    assert_eq!(durable(&rx, expected.len()), expected);
    let live = log.append(&empty("step_started"), None, None).unwrap();
    assert_eq!(next(&rx), Some(live));
}

#[test]
fn a_catch_up_over_an_unreadable_line_returns_the_lines_before_it_then_the_error() {
    let (tmp, log, _) = steps("watch-catchup-bad", 3);
    let mut watcher = log.watch();
    // Nobody drains while the flood is appended, so the queue holds its
    // first `CAPACITY` lines and drops the rest; the catch-up re-reads the
    // lines after them from the log.
    let written: Vec<Envelope> = (0..CAPACITY + 100)
        .map(|_| log.append(&empty("step_started"), None, None).unwrap())
        .collect();
    corrupt(&tmp.session(&id("s_1")), CAPACITY + 50);
    // The corrupt line is at file index `CAPACITY + 50`; the watcher began
    // at file index 3, so every line before it is `written[..CAPACITY + 47]`.
    let mut got = Vec::new();
    let err = loop {
        match watcher.try_recv() {
            Ok(Some(line)) => got.push(line),
            Ok(None) => panic!("the catch-up ended before its read error"),
            Err(err) => break err,
        }
    };
    assert_eq!(got, written[..CAPACITY + 47]);
    assert!(
        err.to_string().contains(&format!("line {}", CAPACITY + 51)),
        "{err}"
    );
    assert_eq!(err.code(), contract::ErrorCode::LogCorrupt);
    assert_eq!(watcher.try_recv().unwrap(), None);
}

#[test]
fn a_range_over_a_log_cut_short_is_an_error_with_no_lines() {
    let (tmp, log, written) = steps("range-cut", 6);
    let dir = tmp.session(&id("s_1"));
    let whole = fs::read(dir.join("events.jsonl")).unwrap();
    let mut start = 0;
    for line in whole.split_inclusive(|b| *b == b'\n').take(3) {
        start += line.len();
    }
    let len = whole[start..].iter().position(|b| *b == b'\n').unwrap();
    let _cut = truncate(&dir, 3, len / 2);
    // A window holding the cut is an error with no lines; a window before
    // it still reads.
    let err = log.range(0, 6).unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::IoFailed);
    assert_eq!(log.range(0, 2).unwrap(), written[..2]);
}
