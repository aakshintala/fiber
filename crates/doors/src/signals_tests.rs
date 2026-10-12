//! The signals thread's decisions, every branch without a real signal, the
//! bound on a fake clock, and one real SIGTERM to a child process.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::io::Read as _;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
use std::os::unix::process::CommandExt as _;

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

/// Makes the FIFO at `path` through a `mkfifo` child in its own process
/// group under a watchdog: the group is killed and reaped on every path,
/// so a stuck `mkfifo` never keeps running. The worst case is two bounds.
fn mkfifo(path: &Path) {
    let mut command = Command::new("mkfifo");
    command
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.process_group(0);
    let mut child = command.spawn().expect("mkfifo started");
    let group = child.id();
    let (finished_tx, finished) = mpsc::channel();
    thread::spawn(move || if let Ok(()) = finished_tx.send(child.wait()) {});
    // The child is already spawned, so a watchdog that cannot start must
    // still leave nothing running: kill the group and wait for the waiter
    // before the panic continues.
    let watchdog =
        match std::panic::catch_unwind(AssertUnwindSafe(|| fakes::Watchdog::group(group))) {
            Ok(watchdog) => watchdog,
            Err(payload) => {
                match fakes::kill_group(group, "KILL") {
                    Ok(_) | Err(_) => {}
                }
                match Deadline::after(CHILD_WITHIN).recv(&finished) {
                    Ok(_) | Err(_) => {}
                }
                std::panic::resume_unwind(payload);
            }
        };
    match Deadline::after(CHILD_WITHIN).recv(&finished) {
        Ok(status) => {
            let cleanup = Deadline::after(CHILD_WITHIN);
            assert!(
                status.expect("mkfifo waited").success(),
                "mkfifo made {path:?}"
            );
            watchdog.stand_down(cleanup.left());
        }
        Err(_) => {
            let cleanup = Deadline::after(CHILD_WITHIN);
            match fakes::kill_group(group, "KILL") {
                Ok(_) | Err(_) => {}
            }
            let reaped = cleanup.recv(&finished).is_ok();
            let emptied = fakes::group_empties(group, cleanup.left());
            panic!("waited {CHILD_WITHIN:?} for mkfifo (reaped: {reaped}, group empty: {emptied})");
        }
    }
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    let mark = clock.advance_marked(KILL_AT.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(
        clock.await_parked_since(&mark, Some(until), DEADLINE),
        "short of the bound it waits again"
    );
    assert!(calls.try_recv().is_err(), "nothing killed before the bound");
    assert!(did.try_recv().is_err(), "no exit before the bound");
    clock.advance(Duration::from_millis(1));
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
    assert!(
        clock.await_parked(kill_at, DEADLINE),
        "the bound waits at the kill point"
    );
    let mark = clock.advance_marked(KILL_AT.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(
        clock.await_parked_since(&mark, Some(kill_at), DEADLINE),
        "short of the kill point it waits again"
    );
    assert!(
        calls.try_recv().is_err(),
        "nothing killed before the kill point"
    );
    clock.advance(Duration::from_millis(1));
    let ended = Deadline::after(DEADLINE);
    assert_eq!(
        ended.recv(&calls).unwrap(),
        (Did::Bound, String::new()),
        "the kill comes before the line"
    );
    let (did, text) = ended.recv(&calls).unwrap();
    assert_eq!(did, Did::Exit(143));
    assert_eq!(
        text,
        "{\"ts\":1700000004900,\"level\":\"error\",\"process\":\"ask\",\"code\":\"timeout\",\"message\":\"The shutdown reached its 4.9-second kill point; every process group still alive was killed and the process exits 143.\"}\n"
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
    mkfifo(&fifo);
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
    assert!(
        clock.await_parked(kill_at, DEADLINE),
        "the bound waits at the kill point"
    );
    clock.advance(KILL_AT);
    assert!(
        clock.await_parked(kill_at + Duration::from_millis(60), DEADLINE),
        "the slow kill waits out its 60 ms from the kill point"
    );
    clock.advance(Duration::from_millis(60));
    assert_eq!(Deadline::after(DEADLINE).recv(&calls).unwrap(), Did::Bound);
    assert!(
        clock.await_parked(exit_by, DEADLINE),
        "the end waits for the line until 5 s"
    );
    let mark = clock.advance_marked(Duration::from_millis(39));
    assert!(
        clock.await_parked_since(&mark, Some(exit_by), DEADLINE),
        "short of 5 s it waits again"
    );
    assert!(did.try_recv().is_err(), "no exit before 5 s");
    clock.advance(Duration::from_millis(1));
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(KILL_AT);
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(KILL_AT);
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(KILL_AT);
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(KILL_AT);
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(KILL_AT);
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(KILL_AT);
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(143));
}

/// The child's marker: set, the test installs the signals and sends itself
/// SIGTERM while booting.
const CHILD: &str = "FIBER_DOORS_SIGNALS_CHILD";

/// How long the real-SIGTERM child may run, and how long `mkfifo` may
/// run: the wait is this long, then the cleanup is this long again, so
/// the worst case is twice this.
const CHILD_WITHIN: Duration = Duration::from_secs(5);

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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(KILL_AT);
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
    assert!(
        !home
            .join("logs")
            .join(format!("ask-{}.log", std::process::id()))
            .exists(),
        "nothing lands in the pid-named file"
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(KILL_AT);
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
        let _waited = Deadline::after(CHILD_WITHIN).recv(&never);
        std::process::exit(0);
    }
    let name = module_path!().split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::a_real_sigterm_while_booting_exits_143"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait().unwrap()));
    let Ok(status) = Deadline::after(DEADLINE).recv(&rx) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {DEADLINE:?} for the child to exit on SIGTERM");
    };
    assert_eq!(status.code(), Some(143), "{status}");
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
    let (bound_tx, bound_calls) = mpsc::channel();
    let bound_home: PathBuf = home.to_path_buf();
    signals.arm(
        Box::new(move || record_tx.send(Did::Signal(-1)).unwrap()),
        Box::new(move || bound_tx.send((Did::Bound, log_text(&bound_home))).unwrap()),
    );
    let start = clock.now();
    let (worker_tx, worker_rx) = mpsc::channel();
    thread::spawn(move || {
        signals.handle(SIGTERM);
        if let Ok(()) = worker_tx.send(()) {}
    });
    let waited = Deadline::after(DEADLINE);
    assert_eq!(
        waited.recv(&bound_calls).unwrap(),
        (Did::Bound, String::new())
    );
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(KILL_AT);
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(KILL_AT);
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound counts from the in-write signal"
    );
    clock.advance(KILL_AT);
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
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound counts from the in-write close"
    );
    clock.advance(KILL_AT);
    let ended = Deadline::after(DEADLINE);
    assert_eq!(ended.recv(&calls).unwrap(), Did::Bound);
    assert_eq!(ended.recv(&did).unwrap(), Did::Exit(0));
}
