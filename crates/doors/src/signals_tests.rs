//! The signals thread's decisions, every branch without a real signal, the
//! bound on a fake clock, and one real SIGTERM to a child process.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::SessionId;
use contract::clock::Clock as _;
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::diag::{Diag, Level, Process};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

use super::{
    Action, KILL_AT, LINE_CAP, Phase, SHUTDOWN_BOUND, Signals, decide, signal_code, sleep_until,
};

#[allow(
    clippy::duplicate_mod,
    reason = "each unit-test file includes the shared support itself"
)]
#[path = "../tests/support/mod.rs"]
mod support;

use support::DEADLINE;

#[test]
fn each_signal_has_its_exit_code() {
    assert_eq!(signal_code(SIGTERM), 143);
    assert_eq!(signal_code(SIGINT), 130);
    assert_eq!(signal_code(SIGHUP), 129);
}

#[test]
fn committing_defers_the_first_signal_and_a_second_term_or_int_kills_the_groups() {
    for (signal, code) in [(SIGTERM, 143), (SIGINT, 130), (SIGHUP, 129)] {
        assert_eq!(
            decide(Phase::Committing { deferred: None }, signal, 0),
            Action::Defer(code)
        );
    }
    assert_eq!(
        decide(Phase::Committing { deferred: None }, SIGTERM, 1),
        Action::KillGroups
    );
    assert_eq!(
        decide(
            Phase::Committing {
                deferred: Some(143)
            },
            SIGINT,
            2
        ),
        Action::KillGroups
    );
    // The doc names only SIGTERM and SIGINT for a second signal.
    assert_eq!(
        decide(
            Phase::Committing {
                deferred: Some(129)
            },
            SIGHUP,
            1
        ),
        Action::Nothing
    );
}

/// Everything the signals did, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Did {
    Exit(i32),
    Bound,
    Signal(i32),
    Second,
}

fn recorded(clock: &Arc<FakeClock>) -> (Arc<Signals>, Receiver<Did>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let exit = Box::new(move |code| tx.lock().unwrap().send(Did::Exit(code)).unwrap());
    let clock: Arc<dyn contract::clock::Clock> = clock.clone();
    (Arc::new(Signals::new(clock, exit, None)), rx)
}

fn sender(tx: &Sender<Did>) -> Mutex<Sender<Did>> {
    Mutex::new(tx.clone())
}

fn arm(signals: &Signals, did: &Sender<Did>) {
    arm_with(signals, did, did);
}

fn arm_with(signals: &Signals, record_did: &Sender<Did>, bound_did: &Sender<Did>) {
    let record_tx = sender(record_did);
    let bound_tx = sender(bound_did);
    signals.arm(
        Box::new(move || record_tx.lock().unwrap().send(Did::Signal(-1)).unwrap()),
        Box::new(move || bound_tx.lock().unwrap().send(Did::Bound).unwrap()),
    );
}

fn start(signals: &Signals, did: &Sender<Did>) -> Option<i32> {
    let on_signal = sender(did);
    let on_second = sender(did);
    let started = signals.start(
        Box::new(move |code| on_signal.lock().unwrap().send(Did::Signal(code)).unwrap()),
        Box::new(move || on_second.lock().unwrap().send(Did::Second).unwrap()),
    );
    if started.is_some() {
        return started;
    }
    signals.commit(|| ()).err()
}

fn start_only(signals: &Signals, did: &Sender<Did>) -> Option<i32> {
    let on_signal = sender(did);
    let on_second = sender(did);
    signals.start(
        Box::new(move |code| on_signal.lock().unwrap().send(Did::Signal(code)).unwrap()),
        Box::new(move || on_second.lock().unwrap().send(Did::Second).unwrap()),
    )
}

/// A diagnostic writer for `home` a test reads back: an `ask` process with
/// no session yet, so its file names the process id.
fn writer(home: &Path, clock: &Arc<FakeClock>) -> Arc<Diag> {
    let timed: Arc<dyn contract::clock::Clock> = clock.clone();
    Arc::new(Diag::new(home, Process::Ask, Level::Info, timed))
}

/// Every diagnostic line under `home`'s `logs/`, as `(file, line)` in file
/// order: the whole list, so a missing, extra or misfiled line fails the
/// assertion. Each line's `ts` reads as a positive integer; the caller
/// compares the rest whole. An absent `logs/` holds no lines.
fn diag_lines(home: &Path) -> Vec<(String, serde_json::Value)> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(home.join("logs")) {
        for entry in entries {
            names.push(entry.unwrap().file_name().into_string().unwrap());
        }
    }
    names.sort();
    let mut lines = Vec::new();
    for name in names {
        let text = std::fs::read_to_string(home.join("logs").join(&name)).unwrap();
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(
                value
                    .get("ts")
                    .and_then(|ts| ts.as_u64())
                    .is_some_and(|ts| ts > 0),
                "ts is a positive integer: {line}"
            );
            lines.push((name.clone(), value));
        }
    }
    lines
}

/// `line` without its `ts`: the clock's reading is pinned by the positive
/// check in [`diag_lines`], so assertions compare the rest whole.
fn without_ts(mut line: serde_json::Value) -> serde_json::Value {
    line.as_object_mut().unwrap().remove("ts");
    line
}

/// The early line a pre-`fiber_started` `signal` leaves: level `error`,
/// no `session_id`, code `signal`, naming the signal and the exit code.
fn early_line(signal: &str, code: i32) -> serde_json::Value {
    serde_json::json!({
        "level": "error",
        "process": "ask",
        "code": "signal",
        "message": format!(
            "{signal} arrived before the session started; the process exits {code}."
        ),
    })
}

/// The bound line in `home`'s pid-named file, or nothing when the file is
/// absent. Never called on a FIFO, whose open would block with no reader.
fn log_text(home: &Path) -> String {
    let path = home
        .join("logs")
        .join(format!("ask-{}.log", std::process::id()));
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Arms snapshot signals: the armed and bound recorders send what the line
/// holds so far with what they record.
fn arm_snapshotting(signals: &Signals, home: &Path, did: &Sender<(Did, String)>) {
    let record_tx = Mutex::new(did.clone());
    let bound_tx = Mutex::new(did.clone());
    let record_home: PathBuf = home.to_path_buf();
    let bound_home: PathBuf = home.to_path_buf();
    signals.arm(
        Box::new(move || {
            record_tx
                .lock()
                .unwrap()
                .send((Did::Signal(-1), log_text(&record_home)))
                .unwrap();
        }),
        Box::new(move || {
            bound_tx
                .lock()
                .unwrap()
                .send((Did::Bound, log_text(&bound_home)))
                .unwrap();
        }),
    );
}

/// Signals with a held writer whose bound and exit recorders also send the
/// line written so far, and the channel they send on.
fn snapshotting(
    clock: &Arc<FakeClock>,
    home: &Path,
    diag: Arc<Diag>,
) -> (Arc<Signals>, Receiver<(Did, String)>) {
    let (tx, rx) = mpsc::channel();
    let exit_tx = Mutex::new(tx.clone());
    let owned: PathBuf = home.to_path_buf();
    let exit = Box::new(move |code| {
        exit_tx
            .lock()
            .unwrap()
            .send((Did::Exit(code), log_text(&owned)))
            .unwrap();
    });
    let timed: Arc<dyn contract::clock::Clock> = clock.clone();
    let signals = Arc::new(Signals::new(timed, exit, Some(diag)));
    arm_snapshotting(&signals, home, &tx);
    (signals, rx)
}

/// Arms signals whose kill stands in a slow one: it waits `takes` on the
/// fake clock, where a real kill would run, then records the bound.
fn arm_slow_kill(
    signals: &Signals,
    record_did: &Sender<Did>,
    bound_did: &Sender<Did>,
    clock: Arc<FakeClock>,
    takes: Duration,
) {
    let record_tx = sender(record_did);
    let bound_tx = sender(bound_did);
    signals.arm(
        Box::new(move || record_tx.lock().unwrap().send(Did::Signal(-1)).unwrap()),
        Box::new(move || {
            sleep_until(clock.as_ref(), clock.now() + takes);
            bound_tx.lock().unwrap().send(Did::Bound).unwrap();
        }),
    );
}

#[test]
fn a_signal_while_booting_exits_with_its_code() {
    for (signal, code) in [(SIGTERM, 143), (SIGINT, 130), (SIGHUP, 129)] {
        let clock = FakeClock::new();
        let (signals, did) = recorded(&clock);
        signals.handle(signal);
        assert_eq!(did.try_recv().unwrap(), Did::Exit(code));
    }
}

#[test]
fn a_signal_while_armed_is_returned_by_start_and_never_shuts_down() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    signals.handle(SIGHUP);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(-1));
    signals.handle(SIGTERM);
    assert!(
        calls.try_recv().is_err(),
        "a second signal while armed does nothing"
    );
    assert_eq!(start(&signals, &tx), Some(129));
    // Still armed: a later signal neither shuts down nor exits.
    signals.handle(SIGINT);
    assert!(calls.try_recv().is_err(), "no shutdown ran");
    assert!(did.try_recv().is_err(), "nothing exited before the bound");
}

#[test]
fn a_signal_once_started_shuts_down_and_a_second_kills_the_groups() {
    let clock = FakeClock::new();
    let (signals, _did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    signals.handle(SIGTERM);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(143));
    signals.handle(SIGHUP);
    assert!(calls.try_recv().is_err(), "a second SIGHUP does nothing");
    signals.handle(SIGINT);
    assert_eq!(calls.try_recv().unwrap(), Did::Second);
}

#[test]
fn the_bound_s_moments_are_the_doc_s() {
    assert_eq!(SHUTDOWN_BOUND, Duration::from_secs(5));
    assert_eq!(KILL_AT, Duration::from_millis(4_900));
    assert_eq!(LINE_CAP, Duration::from_millis(100));
}

#[test]
fn the_bound_kills_and_exits_at_4_9_seconds_with_no_writer() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    let until = clock.now() + KILL_AT;
    signals.handle(SIGTERM);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(-1));
    let mark = clock.advance_when_parked(
        Some(until),
        None,
        KILL_AT.checked_sub(Duration::from_millis(1)).unwrap(),
        &Deadline::after(DEADLINE),
    );
    assert!(
        clock.await_parked_since(&mark, Some(until), fakes::Deadline::after(DEADLINE)),
        "short of the bound it waits again"
    );
    assert!(calls.try_recv().is_err(), "nothing killed before the bound");
    assert!(did.try_recv().is_err(), "no exit before the bound");
    clock.advance_when_parked(
        Some(until),
        None,
        Duration::from_millis(1),
        &Deadline::after(DEADLINE),
    );
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(143));
}

#[test]
fn the_bound_kills_at_4_9_seconds_then_writes_its_line_then_exits() {
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let diag = writer(&home, &clock);
    let (signals, calls) = snapshotting(&clock, &home, diag);
    let start = clock.now();
    signals.handle(SIGTERM);
    assert_eq!(calls.try_recv().unwrap(), (Did::Signal(-1), String::new()));
    let kill_at = start + KILL_AT;
    let mark = clock.advance_when_parked(
        Some(kill_at),
        None,
        KILL_AT.checked_sub(Duration::from_millis(1)).unwrap(),
        &Deadline::after(DEADLINE),
    );
    assert!(
        clock.await_parked_since(&mark, Some(kill_at), fakes::Deadline::after(DEADLINE)),
        "short of the kill point it waits again"
    );
    assert!(
        calls.try_recv().is_err(),
        "nothing killed before the kill point"
    );
    clock.advance_when_parked(
        Some(kill_at),
        None,
        Duration::from_millis(1),
        &Deadline::after(DEADLINE),
    );
    let ended = Deadline::after(DEADLINE);
    assert_eq!(
        ended.recv(&calls).unwrap(),
        (
            Did::Bound,
            "{\"ts\":1700000000000,\"level\":\"error\",\"process\":\"ask\",\"code\":\"signal\",\"message\":\"SIGTERM arrived before the session started; the process exits 143.\"}\n".to_owned()
        ),
        "the kill comes after the early line"
    );
    let (did, text) = ended.recv(&calls).unwrap();
    assert_eq!(did, Did::Exit(143));
    assert_eq!(
        text,
        "{\"ts\":1700000000000,\"level\":\"error\",\"process\":\"ask\",\"code\":\"signal\",\"message\":\"SIGTERM arrived before the session started; the process exits 143.\"}\n{\"ts\":1700000004900,\"level\":\"error\",\"process\":\"ask\",\"code\":\"timeout\",\"message\":\"The shutdown reached its 4.9-second kill point; every process group still alive was killed and the process exits 143.\"}\n"
    );
    assert_eq!(clock.now(), start + Duration::from_millis(4_900));
}

#[test]
fn a_stalled_bound_line_still_exits_at_5_seconds() {
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    std::fs::create_dir_all(home.join("logs")).unwrap();
    let fifo = home
        .join("logs")
        .join(format!("ask-{}.log", std::process::id()));
    fakes::fifo(&fifo);
    let clock = FakeClock::new();
    let diag = writer(&home, &clock);
    let timed: Arc<dyn contract::clock::Clock> = clock.clone();
    let (exit_tx, did) = mpsc::channel();
    let exit = Box::new(move |code| exit_tx.send(Did::Exit(code)).unwrap());
    let signals = Arc::new(Signals::new(timed, exit, Some(diag)));
    let (tx, calls) = mpsc::channel();
    arm_slow_kill(&signals, &tx, &tx, clock.clone(), Duration::from_millis(60));
    assert_eq!(start(&signals, &tx), None);
    let start_at = clock.now();
    signals.handle(SIGTERM);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(143));
    let kill_at = start_at + KILL_AT;
    let exit_by = start_at + SHUTDOWN_BOUND;
    clock.advance_when_parked(Some(kill_at), None, KILL_AT, &Deadline::after(DEADLINE));
    clock.advance_when_parked(
        Some(kill_at + Duration::from_millis(60)),
        None,
        Duration::from_millis(60),
        &Deadline::after(DEADLINE),
    );
    assert_eq!(Deadline::after(DEADLINE).recv(&calls).unwrap(), Did::Bound);
    let mark = clock.advance_when_parked(
        Some(exit_by),
        None,
        Duration::from_millis(39),
        &Deadline::after(DEADLINE),
    );
    assert!(
        clock.await_parked_since(&mark, Some(exit_by), fakes::Deadline::after(DEADLINE)),
        "short of 5 s it waits again"
    );
    assert!(did.try_recv().is_err(), "no exit before 5 s");
    clock.advance_when_parked(
        Some(exit_by),
        None,
        Duration::from_millis(1),
        &Deadline::after(DEADLINE),
    );
    assert_eq!(
        Deadline::after(DEADLINE).recv(&did).unwrap(),
        Did::Exit(143)
    );
    assert_eq!(clock.now(), start_at + Duration::from_secs(5));
    // The line is still stuck in the FIFO's open: read it out.
    let (fifo_tx, fifo_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        let read = std::fs::File::open(&fifo)
            .and_then(|mut file| file.read_to_string(&mut text))
            .map(|_| text);
        if let Ok(()) = fifo_tx.send(read) {}
    });
    let text = Deadline::after(DEADLINE)
        .recv(&fifo_rx)
        .expect("the line reads")
        .expect("the FIFO reads");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one line");
    let line: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(line["level"], "error");
    assert_eq!(line["process"], "ask");
    assert!(line.get("session_id").is_none());
    assert_eq!(line["code"], "timeout");
    assert_eq!(
        line["message"],
        "The shutdown reached its 4.9-second kill point; every process group still alive was killed and the process exits 143."
    );
    let ts = line["ts"].as_u64().unwrap();
    assert!(
        (1_700_000_004_960..=1_700_000_005_000).contains(&ts),
        "ts {ts} is while the test advances past the kill point"
    );
}

#[test]
fn a_started_shutdown_has_the_same_bound() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    signals.handle(SIGHUP);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(129));
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(129));
}

/// A reader that fails at once, as a broken lifeline does.
struct Broken;

impl std::io::Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("the parent is gone"))
    }
}

/// A lifeline reader that blocks until the test releases it with EOF.
struct Held {
    /// Sent on entering `read`, so the test knows the lifeline thread is
    /// blocked inside it.
    entered: mpsc::Sender<()>,
    /// The test sends once to release the read as EOF.
    release: mpsc::Receiver<()>,
}

impl std::io::Read for Held {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        let _sent = self.entered.send(());
        match self.release.recv() {
            Ok(()) => Ok(0),
            Err(_) => Err(std::io::Error::other("the test ended before releasing")),
        }
    }
}

#[test]
fn a_lifeline_eof_after_start_takes_the_sighup_path() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    // Bytes the parent writes are ignored; EOF ends the lifeline.
    signals.lifeline(Box::new(std::io::Cursor::new(b"hello".to_vec())));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&calls).unwrap(),
        Did::Signal(129),
        "EOF after start shuts down with the hangup code"
    );
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(129));
}

#[test]
fn a_blocked_lifeline_triggers_nothing_until_released() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    signals.lifeline(Box::new(Held {
        entered: entered_tx,
        release: release_rx,
    }));
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("the lifeline blocks reading stdin: a skipped drain never reads");
    assert!(
        calls.try_recv().is_err(),
        "no shutdown while the lifeline is blocked"
    );
    assert!(
        did.try_recv().is_err(),
        "no exit while the lifeline is blocked"
    );
    release_tx.send(()).unwrap();
    assert_eq!(
        Deadline::after(DEADLINE).recv(&calls).unwrap(),
        Did::Signal(129),
        "EOF after the block shuts down with the hangup code"
    );
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(129));
}

#[test]
fn a_lifeline_read_error_takes_the_same_path() {
    let clock = FakeClock::new();
    let (signals, _did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    signals.lifeline(Box::new(Broken));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&calls).unwrap(),
        Did::Signal(129),
        "a read error after start shuts down too"
    );
}

#[test]
fn a_lifeline_eof_while_booting_exits_129_at_once() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    signals.lifeline(Box::new(std::io::Cursor::new(Vec::new())));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&did).unwrap(),
        Did::Exit(129),
        "EOF while booting exits at once, as a real SIGHUP does"
    );
}

#[test]
fn a_lifeline_eof_while_armed_is_recorded_for_start() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    signals.lifeline(Box::new(std::io::Cursor::new(Vec::new())));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&calls).unwrap(),
        Did::Signal(-1),
        "EOF while armed records the hangup"
    );
    assert_eq!(start(&signals, &tx), Some(129));
    assert!(did.try_recv().is_err(), "nothing exited before the bound");
}

#[test]
fn a_signal_while_booting_does_not_run_on_record() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    signals.handle(SIGTERM);
    assert_eq!(did.try_recv().unwrap(), Did::Exit(143));
    let (record_tx, record_calls) = mpsc::channel();
    let (bound_tx, bound_calls) = mpsc::channel();
    arm_with(&signals, &record_tx, &bound_tx);
    signals.handle(SIGINT);
    assert!(
        record_calls.try_recv().is_err(),
        "a booting exit is not recorded"
    );
    assert_eq!(start(&signals, &bound_tx), None);
    drop(bound_calls);
}

#[test]
fn a_first_signal_once_started_does_not_run_on_record() {
    let clock = FakeClock::new();
    let (signals, _did) = recorded(&clock);
    let (record_tx, record_calls) = mpsc::channel();
    let (bound_tx, bound_calls) = mpsc::channel();
    arm_with(&signals, &record_tx, &bound_tx);
    assert_eq!(start(&signals, &bound_tx), None);
    signals.handle(SIGTERM);
    assert_eq!(bound_calls.try_recv().unwrap(), Did::Signal(143));
    assert!(
        record_calls.try_recv().is_err(),
        "a started shutdown runs no on_record"
    );
}

#[test]
fn close_now_while_booting_does_nothing_and_while_armed_records_0() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    signals.close_now();
    assert!(calls.try_recv().is_err(), "no shutdown ran while booting");
    assert!(did.try_recv().is_err(), "nothing exited while booting");
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    signals.handle(SIGTERM);
    assert_eq!(
        calls.try_recv().unwrap(),
        Did::Signal(143),
        "close_now while booting leaves the first signal available"
    );

    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    let on_signal = sender(&tx);
    let on_second = sender(&tx);
    assert_eq!(
        signals.start(
            Box::new(move |code| on_signal.lock().unwrap().send(Did::Signal(code)).unwrap()),
            Box::new(move || on_second.lock().unwrap().send(Did::Second).unwrap()),
        ),
        None,
        "the session stays armed until the commit"
    );
    signals.close_now();
    assert_eq!(
        calls.try_recv().unwrap(),
        Did::Signal(-1),
        "a close with now while armed runs on_record"
    );
    assert!(did.try_recv().is_err(), "nothing exited while armed");
    assert_eq!(signals.commit(|| ()), Err(0));
    signals.handle(SIGTERM);
    assert!(
        calls.try_recv().is_err(),
        "a signal after the recorded close does nothing"
    );
    assert!(did.try_recv().is_err(), "nothing exited before the bound");
}

#[test]
fn close_now_once_started_shuts_down_with_code_0_and_the_bound_exits_0() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    signals.close_now();
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(0));
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(0));
}

#[test]
fn a_second_close_now_does_nothing_and_a_later_term_kills_the_groups() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    signals.close_now();
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(0));
    signals.close_now();
    assert!(calls.try_recv().is_err(), "a second close_now does nothing");
    signals.handle(SIGHUP);
    assert!(calls.try_recv().is_err(), "a later SIGHUP does nothing");
    signals.handle(SIGTERM);
    assert_eq!(calls.try_recv().unwrap(), Did::Second);
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(0));
    assert!(did.try_recv().is_err(), "no second bound ran");
    assert!(calls.try_recv().is_err(), "no second bound ran");
}

#[test]
fn close_now_after_a_signal_keeps_the_signal_and_its_code() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    signals.handle(SIGTERM);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(143));
    signals.close_now();
    assert!(calls.try_recv().is_err(), "close_now keeps the signal");
    let until = clock.now() + KILL_AT;
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(143));
}

/// The child's marker: set, the test installs the signals and sends itself
/// SIGTERM while booting.
const CHILD: &str = "FIBER_DOORS_SIGNALS_CHILD";

/// After `attach` the bound line lands in the session-named file with
/// `session_id` and nothing lands in the pid-named one; with no `attach`
/// it lands in the pid-named file, saying the process's own exit code.
#[test]
fn after_attach_the_bound_line_names_the_session_and_the_early_line_the_pid() {
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let diag = writer(&home, &clock);
    let timed: Arc<dyn contract::clock::Clock> = clock.clone();
    let (exit_tx, did) = mpsc::channel();
    let exit = Box::new(move |code| exit_tx.send(code).unwrap());
    let signals = Arc::new(Signals::new(timed, exit, Some(diag)));
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    signals.attach(SessionId("s_0123456789abcdef".into()));
    assert_eq!(start_only(&signals, &tx), None);
    signals.handle(SIGTERM);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(-1));
    let until = clock.now() + KILL_AT;
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    assert_eq!(Deadline::after(DEADLINE).recv(&did).unwrap(), 143);
    let text =
        std::fs::read_to_string(home.join("logs").join("ask-s_0123456789abcdef.log")).unwrap();
    assert_eq!(text.lines().count(), 1, "exactly one line");
    let line: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(line["level"], "error");
    assert_eq!(line["process"], "ask");
    assert_eq!(line["session_id"], "s_0123456789abcdef");
    assert_eq!(line["code"], "timeout");
    assert!(
        line["message"].as_str().unwrap().contains("exits 143"),
        "{}",
        line["message"]
    );
    // The early line stays pid-named: `attach` routes only the bound line
    // to the session file.
    let early = std::fs::read_to_string(
        home.join("logs")
            .join(format!("ask-{}.log", std::process::id())),
    )
    .unwrap();
    assert_eq!(early.lines().count(), 1, "exactly one early line");
    let line: serde_json::Value = serde_json::from_str(early.trim_end()).unwrap();
    assert_eq!(line["level"], "error");
    assert_eq!(line["process"], "ask");
    assert!(line.get("session_id").is_none());
    assert_eq!(line["code"], "signal");
    assert_eq!(
        line["message"],
        "SIGTERM arrived before the session started; the process exits 143."
    );

    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let diag = writer(&home, &clock);
    let timed: Arc<dyn contract::clock::Clock> = clock.clone();
    let (exit_tx, did) = mpsc::channel();
    let exit = Box::new(move |code| exit_tx.send(code).unwrap());
    let signals = Arc::new(Signals::new(timed, exit, Some(diag)));
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    signals.close_now();
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(0));
    let until = clock.now() + KILL_AT;
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    assert_eq!(Deadline::after(DEADLINE).recv(&did).unwrap(), 0);
    let text = std::fs::read_to_string(
        home.join("logs")
            .join(format!("ask-{}.log", std::process::id())),
    )
    .unwrap();
    assert_eq!(text.lines().count(), 1, "exactly one line");
    let line: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(line["level"], "error");
    assert_eq!(line["process"], "ask");
    assert!(line.get("session_id").is_none());
    assert_eq!(line["code"], "timeout");
    assert!(
        line["message"].as_str().unwrap().contains("exits 0"),
        "{}",
        line["message"]
    );
    assert!(
        !home
            .join("logs")
            .join("ask-s_0123456789abcdef.log")
            .exists(),
        "no session file without attach"
    );
}

#[test]
fn a_real_sigterm_while_booting_exits_143() {
    if std::env::var_os(CHILD).is_some() {
        let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
        let _signals = Signals::install(clock, None).unwrap();
        fakes::kill_pid(std::process::id(), "TERM").unwrap();
        // The signals thread exits the process; this wait only bounds a
        // child whose exit never came.
        let (_held, never) = mpsc::channel::<()>();
        let _waited = Deadline::after(DEADLINE).recv(&never);
        std::process::exit(0);
    }
    let name = module_path!().split_once("::").unwrap().1;
    let output = fakes::rerun_within(
        &format!("{name}::a_real_sigterm_while_booting_exits_143"),
        &[(CHILD, "1")],
        Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(output.status.code(), Some(143), "{}", output.status);
}

#[test]
fn a_bound_that_cannot_start_its_thread_ends_at_once() {
    let clock = FakeClock::new();
    let (tx, did) = mpsc::channel();
    let exit_tx = Mutex::new(tx.clone());
    let exit = Box::new(move |code| exit_tx.lock().unwrap().send(Did::Exit(code)).unwrap());
    let timed: Arc<dyn contract::clock::Clock> = clock.clone();
    let mut signals = Signals::new(timed, exit, None);
    signals.spawn = Box::new(|_| Err(std::io::Error::other("no thread for the test")));
    let signals = Arc::new(signals);
    let (record_tx, record_calls) = mpsc::channel();
    arm_with(&signals, &record_tx, &tx);
    signals.handle(SIGTERM);
    assert_eq!(record_calls.try_recv().unwrap(), Did::Signal(-1));
    // The clock never moved: the bound ran on the signal's own thread.
    assert_eq!(did.try_recv().unwrap(), Did::Bound);
    assert_eq!(did.try_recv().unwrap(), Did::Exit(143));
}

#[test]
fn a_bound_that_cannot_start_its_thread_writes_its_line_then_ends_at_once() {
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let diag = writer(&home, &clock);
    let (exit_tx, did) = mpsc::channel();
    let owned: PathBuf = home.to_path_buf();
    let exit = Box::new(move |code| {
        exit_tx.send((Did::Exit(code), log_text(&owned))).unwrap();
    });
    let timed: Arc<dyn contract::clock::Clock> = clock.clone();
    let mut signals = Signals::new(timed, exit, Some(diag));
    signals.spawn = Box::new(|_| Err(std::io::Error::other("no thread for the test")));
    let signals = Arc::new(signals);
    let (record_tx, record_calls) = mpsc::channel();
    // `end` runs `on_bound` before the timeout append: block there to
    // observe the released readers ahead of the line.
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(release_rx);
    signals.arm(
        Box::new(move || record_tx.send(Did::Signal(-1)).unwrap()),
        Box::new(move || {
            entered_tx.send(()).unwrap();
            release.lock().unwrap().recv().unwrap();
        }),
    );
    let start = clock.now();
    let (worker_tx, worker_rx) = mpsc::channel();
    let worker = Arc::clone(&signals);
    thread::spawn(move || {
        worker.handle(SIGTERM);
        if let Ok(()) = worker_tx.send(()) {}
    });
    let waited = Deadline::after(DEADLINE);
    // The bound ran on the signal's own thread: `end` reached `on_bound`.
    waited.recv(&entered_rx).unwrap();
    // Readers are released before `end` writes: the code is out with no
    // line on disk yet.
    assert_eq!(signals.recorded(), Some(143));
    assert_eq!(
        diag_lines(&home).len(),
        0,
        "no line before the timeout append"
    );
    release_tx.send(()).unwrap();
    let (did, text) = waited.recv(&did).unwrap();
    assert_eq!(did, Did::Exit(143));
    // The clock never moved: the bound ran on the signal's own thread.
    assert_eq!(
        text,
        "{\"ts\":1700000000000,\"level\":\"error\",\"process\":\"ask\",\"code\":\"timeout\",\"message\":\"The shutdown reached its 4.9-second kill point; every process group still alive was killed and the process exits 143.\"}\n"
    );
    waited.recv(&worker_rx).unwrap();
    assert_eq!(record_calls.try_recv().unwrap(), Did::Signal(-1));
    assert_eq!(clock.now(), start);
    // The early line is never written on this path: the timeout line only.
    let lines = diag_lines(&home);
    assert_eq!(lines.len(), 1, "exactly one line: {lines:?}");
    assert_eq!(lines[0].1["code"], "timeout");
}

#[test]
fn recorded_is_the_armed_signal_s_code() {
    let clock = FakeClock::new();
    let (signals, _did) = recorded(&clock);
    assert_eq!(signals.recorded(), None);
    let (tx, _rx) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(signals.recorded(), None);
    signals.handle(SIGTERM);
    assert_eq!(signals.recorded(), Some(signal_code(SIGTERM)));
}

#[test]
fn a_signal_between_start_and_commit_is_recorded_and_nothing_is_written() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start_only(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    signals.handle(SIGTERM);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(-1));
    assert!(
        calls.try_recv().is_err(),
        "no shutdown ran before the commit"
    );
    let mut wrote = false;
    let commit: Result<(), i32> = signals.commit(|| {
        wrote = true;
    });
    assert_eq!(commit, Err(143));
    assert!(!wrote, "nothing is written once a signal was recorded");
    assert_eq!(signals.recorded(), Some(143));
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(143));
}

#[test]
fn a_close_now_between_start_and_commit_is_recorded_with_code_0() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start_only(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    signals.close_now();
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(-1));
    assert!(
        calls.try_recv().is_err(),
        "no shutdown ran before the commit"
    );
    let mut wrote = false;
    let commit: Result<(), i32> = signals.commit(|| {
        wrote = true;
    });
    assert_eq!(commit, Err(0));
    assert!(!wrote, "nothing is written once a close was recorded");
    assert_eq!(signals.recorded(), Some(0));
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(0));
}

#[test]
fn a_signal_during_commit_is_handled_once_the_write_returns() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start_only(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    let commit = signals.commit(|| {
        signals.handle(SIGTERM);
        assert!(
            calls.try_recv().is_err(),
            "no shutdown runs while the write holds the commit"
        );
        signals.handle(SIGTERM);
        assert_eq!(
            calls.try_recv().unwrap(),
            Did::Second,
            "a second signal during the write kills the groups at once"
        );
        7
    });
    assert_eq!(commit, Ok(7));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&calls).unwrap(),
        Did::Signal(143)
    );
    assert!(
        calls.try_recv().is_err(),
        "the deferred signal shuts down exactly once"
    );
    assert_eq!(signals.recorded(), None);
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(143));
}

#[test]
fn a_close_now_during_commit_shuts_down_with_code_0_after_the_write() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start_only(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    let commit = signals.commit(|| {
        signals.close_now();
        assert!(
            calls.try_recv().is_err(),
            "no shutdown runs while the write holds the commit"
        );
        7
    });
    assert_eq!(commit, Ok(7));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&calls).unwrap(),
        Did::Signal(0)
    );
    assert!(
        calls.try_recv().is_err(),
        "the deferred close shuts down exactly once"
    );
    assert_eq!(signals.recorded(), None);
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(0));
}

#[test]
fn close_now_after_a_signal_while_armed_does_nothing() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start_only(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    signals.handle(SIGTERM);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(-1));
    signals.close_now();
    assert!(
        calls.try_recv().is_err(),
        "a close with now after a signal while armed does nothing"
    );
    assert_eq!(signals.recorded(), Some(143));
    assert_eq!(signals.commit(|| ()), Err(143));
    assert!(
        calls.try_recv().is_err(),
        "the recorded close runs no second on_record"
    );
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(143));
    assert!(calls.try_recv().is_err(), "no second bound ran");
    assert!(did.try_recv().is_err(), "no second exit ran");
}

#[test]
fn close_now_after_a_signal_while_committing_does_nothing() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start_only(&signals, &tx), None);
    let until = clock.now() + KILL_AT;
    let commit = signals.commit(|| {
        signals.handle(SIGTERM);
        assert!(
            calls.try_recv().is_err(),
            "the deferred signal runs nothing while the write holds the commit"
        );
        signals.close_now();
        assert!(
            calls.try_recv().is_err(),
            "a close with now after a signal while committing does nothing"
        );
        7
    });
    assert_eq!(commit, Ok(7));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&calls).unwrap(),
        Did::Signal(143),
        "the shutdown keeps the signal's code"
    );
    assert!(
        calls.try_recv().is_err(),
        "the deferred signal shuts down exactly once"
    );
    clock.advance_when_parked(Some(until), None, KILL_AT, &Deadline::after(DEADLINE));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(143));
    assert!(calls.try_recv().is_err(), "no second bound ran");
    assert!(did.try_recv().is_err(), "no second exit ran");
}

/// Signals with a held writer whose exit recorder sends only the code.
fn exiting(clock: &Arc<FakeClock>, home: &Path) -> (Arc<Signals>, Receiver<Did>) {
    let (exit_tx, did) = mpsc::channel();
    let exit = Box::new(move |code| exit_tx.send(Did::Exit(code)).unwrap());
    let timed: Arc<dyn contract::clock::Clock> = clock.clone();
    let signals = Arc::new(Signals::new(timed, exit, Some(writer(home, clock))));
    (signals, did)
}

/// Arms `signals` with an `on_record` that first sends `entered`, then
/// blocks on `release`: the committed pause seam, so the test orders
/// other threads between the publish and the early append. The `Receiver`
/// rides a mutex, as an `Fn` callback must stay `Sync`.
fn arm_held(signals: &Signals, entered: Sender<()>, release: Receiver<()>, bound: Sender<Did>) {
    let entered = Mutex::new(entered);
    let release = Mutex::new(release);
    let bound = Mutex::new(bound);
    signals.arm(
        Box::new(move || {
            entered.lock().unwrap().send(()).unwrap();
            release.lock().unwrap().recv().unwrap();
        }),
        Box::new(move || bound.lock().unwrap().send(Did::Bound).unwrap()),
    );
}

#[test]
fn a_signal_while_booting_writes_one_line_before_it_exits() {
    for (signal, code, name) in [
        (SIGTERM, 143, "SIGTERM"),
        (SIGINT, 130, "SIGINT"),
        (SIGHUP, 129, "SIGHUP"),
    ] {
        let temp = support::Temp::new();
        let home = temp.0.join("h");
        let clock = FakeClock::new();
        let (signals, did) = exiting(&clock, &home);
        signals.handle(signal);
        assert_eq!(
            Deadline::after(DEADLINE).recv(&did).unwrap(),
            Did::Exit(code)
        );
        // The exit fires after the append, so the line is on disk.
        let lines = diag_lines(&home);
        assert_eq!(lines.len(), 1, "exactly one line: {lines:?}");
        assert_eq!(
            lines[0].0,
            format!("ask-{}.log", std::process::id()),
            "the early line stays pid-named"
        );
        assert_eq!(without_ts(lines[0].1.clone()), early_line(name, code));
        assert!(
            lines[0].1.get("session_id").is_none(),
            "no session id before the session exists"
        );
        assert_eq!(signals.recorded(), Some(code));
        // A second booting signal writes nothing more.
        signals.handle(signal);
        assert!(
            did.try_recv().is_err(),
            "a second booting signal exits nothing"
        );
        assert_eq!(diag_lines(&home).len(), 1, "still one line");
    }
}

#[test]
fn a_signal_while_armed_writes_one_line_and_a_second_none() {
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let (signals, did) = exiting(&clock, &home);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    signals.handle(SIGHUP);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(-1));
    assert!(did.try_recv().is_err(), "an armed record exits nothing");
    let lines = diag_lines(&home);
    assert_eq!(lines.len(), 1, "exactly one line: {lines:?}");
    assert_eq!(without_ts(lines[0].1.clone()), early_line("SIGHUP", 129));
    signals.handle(SIGTERM);
    assert!(
        calls.try_recv().is_err(),
        "a second signal while armed does nothing"
    );
    assert!(did.try_recv().is_err(), "nothing exited");
    assert_eq!(diag_lines(&home).len(), 1, "still one line");
    assert_eq!(signals.recorded(), Some(129));
}

#[test]
fn the_lifeline_and_an_armed_race_write_one_line() {
    // The race: SIGTERM reaches pre-append, then the hangup runs while the
    // first line is still held. The hangup funnels into the same `handle`
    // entry the lifeline's worker calls, so a race through `handle` covers
    // the lifeline too.
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let (signals, did) = exiting(&clock, &home);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (bound_tx, bound_calls) = mpsc::channel();
    arm_held(&signals, entered_tx, release_rx, bound_tx);
    let worker = Arc::clone(&signals);
    let (term_done_tx, term_done_rx) = mpsc::channel();
    thread::spawn(move || {
        worker.handle(SIGTERM);
        if let Ok(()) = term_done_tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("SIGTERM reached pre-append");
    let racer = Arc::clone(&signals);
    let (hup_done_tx, hup_done_rx) = mpsc::channel();
    thread::spawn(move || {
        racer.handle(SIGHUP);
        if let Ok(()) = hup_done_tx.send(()) {}
    });
    // The second event is processed while the first line is still held:
    // joining here fails if it blocks behind the line instead.
    Deadline::after(DEADLINE)
        .recv(&hup_done_rx)
        .expect("the hangup ran during the hold");
    drop(bound_calls);
    release_tx.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&term_done_rx)
        .expect("SIGTERM appended its line");
    assert!(did.try_recv().is_err(), "the armed path exits nothing");
    let lines = diag_lines(&home);
    assert_eq!(lines.len(), 1, "exactly one line: {lines:?}");
    assert_eq!(without_ts(lines[0].1.clone()), early_line("SIGTERM", 143));

    // Solo lifeline: end of file while booting takes the SIGHUP path,
    // with the line and the hangup code.
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let (signals, did) = exiting(&clock, &home);
    signals.lifeline(Box::new(std::io::Cursor::new(Vec::new())));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&did).unwrap(),
        Did::Exit(129)
    );
    let lines = diag_lines(&home);
    assert_eq!(lines.len(), 1, "exactly one line: {lines:?}");
    assert_eq!(without_ts(lines[0].1.clone()), early_line("SIGHUP", 129));
}

#[test]
fn no_line_once_started_while_committing_or_on_close_now() {
    // Once started, a signal shuts down with no early line.
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let (signals, did) = exiting(&clock, &home);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start_only(&signals, &tx), None);
    assert!(signals.commit(|| ()).is_ok());
    signals.handle(SIGTERM);
    assert_eq!(
        Deadline::after(DEADLINE).recv(&calls).unwrap(),
        Did::Signal(143)
    );
    assert!(did.try_recv().is_err(), "nothing exited before the bound");
    assert!(diag_lines(&home).is_empty(), "no early line once started");

    // While committing, a first signal defers with no early line: the
    // write closure acks its start before the signal arrives.
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let (signals, did) = exiting(&clock, &home);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start_only(&signals, &tx), None);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = Arc::clone(&signals);
    let (commit_tx, commit_rx) = mpsc::channel();
    thread::spawn(move || {
        let commit: Result<(), i32> = worker.commit(|| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        if let Ok(()) = commit_tx.send(commit) {}
    });
    Deadline::after(DEADLINE)
        .recv(&started_rx)
        .expect("the commit started writing");
    let retraced = Arc::clone(&signals);
    let (hup_done_tx, hup_done_rx) = mpsc::channel();
    thread::spawn(move || {
        retraced.handle(SIGHUP);
        if let Ok(()) = hup_done_tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&hup_done_rx)
        .expect("the committing signal deferred");
    release_tx.send(()).unwrap();
    assert_eq!(
        Deadline::after(DEADLINE).recv(&commit_rx).unwrap(),
        Ok(()),
        "a committing signal still lets the write finish"
    );
    assert_eq!(
        Deadline::after(DEADLINE).recv(&calls).unwrap(),
        Did::Signal(129),
        "the deferred signal shuts down once the write returns"
    );
    assert!(did.try_recv().is_err(), "nothing exited before the bound");
    assert!(
        diag_lines(&home).is_empty(),
        "no early line while committing"
    );

    // `close_now` writes no early line, booting or armed.
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let (signals, did) = exiting(&clock, &home);
    let (tx, calls) = mpsc::channel();
    signals.close_now();
    assert!(calls.try_recv().is_err(), "no shutdown ran while booting");
    assert!(did.try_recv().is_err(), "nothing exited while booting");
    assert_eq!(signals.recorded(), None);
    arm(&signals, &tx);
    signals.close_now();
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(-1));
    assert!(did.try_recv().is_err(), "nothing exited while armed");
    assert_eq!(signals.recorded(), Some(0));
    assert!(diag_lines(&home).is_empty(), "no early line on close_now");
}

#[test]
fn a_recorded_code_waits_for_its_line() {
    // Window 1, armed record: `recorded` and `start` wait while the line
    // is in flight. The `entered` ack proves the handler is held in
    // `on_record`, hence after the publish and before the append; the
    // state read and the empty `logs/` re-check that hold without the
    // clock, and the readers' only wake is the append's notify, so the
    // codes joined after the release necessarily came after their line.
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let timed: Arc<dyn contract::clock::Clock> = clock.clone();
    let (exit_tx, did) = mpsc::channel();
    let exit = Box::new(move |code| exit_tx.send(Did::Exit(code)).unwrap());
    let signals = Arc::new(Signals::new(timed, exit, Some(writer(&home, &clock))));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (bound_tx, bound_calls) = mpsc::channel();
    arm_held(&signals, entered_tx, release_rx, bound_tx);
    let worker = Arc::clone(&signals);
    let (term_done_tx, term_done_rx) = mpsc::channel();
    thread::spawn(move || {
        worker.handle(SIGTERM);
        if let Ok(()) = term_done_tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("SIGTERM reached pre-append");
    let reader = Arc::clone(&signals);
    let (recorded_tx, recorded_rx) = mpsc::channel();
    thread::spawn(
        move || {
            if let Ok(()) = recorded_tx.send(reader.recorded()) {}
        },
    );
    let starter = Arc::clone(&signals);
    let (start_tx, start_rx) = mpsc::channel();
    thread::spawn(move || {
        let voted = starter.start(Box::new(|_| {}), Box::new(|| {}));
        if let Ok(()) = start_tx.send(voted) {}
    });
    // Held in `on_record`: the publish ran and the append has not, so
    // the readers spawned above can only be parked in `settled_state`.
    {
        let state = signals.state.lock().unwrap();
        assert_eq!(state.recorded, Some(143), "the code is published");
        assert_eq!(state.early_pending, Some(143), "its line is in flight");
    }
    assert!(
        diag_lines(&home).is_empty(),
        "no line while the append is held"
    );
    drop(bound_calls);
    release_tx.send(()).unwrap();
    assert_eq!(
        Deadline::after(DEADLINE).recv(&recorded_rx).unwrap(),
        Some(143)
    );
    assert_eq!(
        Deadline::after(DEADLINE).recv(&start_rx).unwrap(),
        Some(143)
    );
    Deadline::after(DEADLINE)
        .recv(&term_done_rx)
        .expect("SIGTERM appended its line");
    assert!(did.try_recv().is_err(), "the armed path exits nothing");
    let lines = diag_lines(&home);
    assert_eq!(lines.len(), 1, "exactly one line: {lines:?}");
    assert_eq!(without_ts(lines[0].1.clone()), early_line("SIGTERM", 143));

    // Window 2, record between `start` and `commit`: `commit` waits while
    // the line is in flight, then reports the code without writing. The
    // `entered` ack orders the hold as in window 1; the state read and the
    // empty `logs/` re-check it without the clock.
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let clock = FakeClock::new();
    let timed: Arc<dyn contract::clock::Clock> = clock.clone();
    let (exit_tx, did) = mpsc::channel();
    let exit = Box::new(move |code| exit_tx.send(Did::Exit(code)).unwrap());
    let signals = Arc::new(Signals::new(timed, exit, Some(writer(&home, &clock))));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (bound_tx, bound_calls) = mpsc::channel();
    arm_held(&signals, entered_tx, release_rx, bound_tx);
    let (tx, calls) = mpsc::channel();
    assert_eq!(start_only(&signals, &tx), None);
    let worker = Arc::clone(&signals);
    let (term_done_tx, term_done_rx) = mpsc::channel();
    thread::spawn(move || {
        worker.handle(SIGTERM);
        if let Ok(()) = term_done_tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("SIGTERM reached pre-append");
    let committer = Arc::clone(&signals);
    let (commit_tx, commit_rx) = mpsc::channel();
    let wrote = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let marking = Arc::clone(&wrote);
    thread::spawn(move || {
        let commit: Result<(), i32> = committer.commit(|| {
            marking.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        if let Ok(()) = commit_tx.send(commit) {}
    });
    // Held in `on_record`, as in window 1: the committer can only be
    // parked in `settled_state` until the append releases it.
    {
        let state = signals.state.lock().unwrap();
        assert_eq!(state.recorded, Some(143), "the code is published");
        assert_eq!(state.early_pending, Some(143), "its line is in flight");
    }
    assert!(
        diag_lines(&home).is_empty(),
        "no line while the append is held"
    );
    drop(bound_calls);
    drop(calls);
    release_tx.send(()).unwrap();
    assert_eq!(
        Deadline::after(DEADLINE).recv(&commit_rx).unwrap(),
        Err(143)
    );
    assert!(
        !wrote.load(std::sync::atomic::Ordering::SeqCst),
        "nothing is written once a signal was recorded"
    );
    Deadline::after(DEADLINE)
        .recv(&term_done_rx)
        .expect("SIGTERM appended its line");
    assert!(did.try_recv().is_err(), "the armed path exits nothing");
    let lines = diag_lines(&home);
    assert_eq!(lines.len(), 1, "exactly one line: {lines:?}");
    assert_eq!(without_ts(lines[0].1.clone()), early_line("SIGTERM", 143));
}

/// The child's marker: set, the test installs the signals with a writer
/// and sends itself SIGTERM while booting.
const WRITER_CHILD: &str = "FIBER_DOORS_SIGNALS_WRITER_CHILD";

/// The child's Fiber home, holding the `logs/` the early line lands in.
const WRITER_HOME: &str = "FIBER_DOORS_SIGNALS_WRITER_HOME";

#[test]
fn install_with_a_writer_records_a_real_sigterm() {
    if std::env::var_os(WRITER_CHILD).is_some() {
        let home = PathBuf::from(std::env::var_os(WRITER_HOME).unwrap());
        let clock = FakeClock::new();
        let timed: Arc<dyn contract::clock::Clock> = clock.clone();
        let _signals = Signals::install(
            timed.clone(),
            Some(Diag::new(&home, Process::Ask, Level::Info, timed)),
        )
        .unwrap();
        fakes::kill_pid(std::process::id(), "TERM").unwrap();
        // The signals thread exits the process; this wait only bounds a
        // child whose exit never came.
        let (_held, never) = mpsc::channel::<()>();
        let _waited = Deadline::after(DEADLINE).recv(&never);
        std::process::exit(0);
    }
    let temp = support::Temp::new();
    let home = temp.0.join("h");
    let held = home.to_str().unwrap().to_owned();
    let name = module_path!().split_once("::").unwrap().1;
    let output = fakes::rerun_within(
        &format!("{name}::install_with_a_writer_records_a_real_sigterm"),
        &[(WRITER_CHILD, "1"), (WRITER_HOME, held.as_str())],
        Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert_eq!(output.status.code(), Some(143), "{}", output.status);
    // The child runs in its own process group under the rerun's watchdog,
    // so no cleanup of its own is needed here.
    let lines = diag_lines(&home);
    assert_eq!(lines.len(), 1, "exactly one line: {lines:?}");
    assert!(
        lines[0]
            .0
            .strip_prefix("ask-")
            .is_some_and(|tail| tail.ends_with(".log")),
        "the early line stays pid-named: {}",
        lines[0].0
    );
    assert_eq!(without_ts(lines[0].1.clone()), early_line("SIGTERM", 143));
    assert!(lines[0].1.get("session_id").is_none());
}
