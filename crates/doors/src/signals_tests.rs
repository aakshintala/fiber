//! The signals thread's decisions, every branch without a real signal, the
//! bound on a fake clock, and one real SIGTERM to a child process.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use fakes::clock::FakeClock;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

use super::{Action, BOUND, Phase, Signals, decide, signal_code};

/// How long a test waits on another thread before it fails.
const DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn each_signal_has_its_exit_code() {
    assert_eq!(signal_code(SIGTERM), 143);
    assert_eq!(signal_code(SIGINT), 130);
    assert_eq!(signal_code(SIGHUP), 129);
}

#[test]
fn booting_exits_at_once_on_any_signal() {
    for (signal, code) in [(SIGTERM, 143), (SIGINT, 130), (SIGHUP, 129)] {
        assert_eq!(decide(Phase::Booting, signal, 0), Action::Exit(code));
    }
}

#[test]
fn armed_records_the_first_signal_and_ignores_the_rest() {
    for (signal, code) in [(SIGTERM, 143), (SIGINT, 130), (SIGHUP, 129)] {
        assert_eq!(decide(Phase::Armed, signal, 0), Action::Record(code));
        assert_eq!(decide(Phase::Armed, signal, 1), Action::Nothing);
    }
}

#[test]
fn started_shuts_down_once_and_a_second_term_or_int_kills_the_groups() {
    for (signal, code) in [(SIGTERM, 143), (SIGINT, 130), (SIGHUP, 129)] {
        assert_eq!(decide(Phase::Started, signal, 0), Action::Shutdown(code));
    }
    assert_eq!(decide(Phase::Started, SIGTERM, 1), Action::KillGroups);
    assert_eq!(decide(Phase::Started, SIGINT, 2), Action::KillGroups);
    // The doc names only SIGTERM and SIGINT for a second signal.
    assert_eq!(decide(Phase::Started, SIGHUP, 1), Action::Nothing);
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
    (Arc::new(Signals::new(clock, exit)), rx)
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
        Box::new(move || {
            record_tx
                .lock()
                .unwrap()
                .send(Did::Signal(-1))
                .unwrap()
        }),
        Box::new(move || bound_tx.lock().unwrap().send(Did::Bound).unwrap()),
    );
}

fn start(signals: &Signals, did: &Sender<Did>) -> Option<i32> {
    let on_signal = sender(did);
    let on_second = sender(did);
    signals.start(
        Box::new(move |code| on_signal.lock().unwrap().send(Did::Signal(code)).unwrap()),
        Box::new(move || on_second.lock().unwrap().send(Did::Second).unwrap()),
    )
}

#[test]
fn a_signal_while_booting_exits_with_its_code() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    signals.handle(SIGINT);
    assert_eq!(did.try_recv().unwrap(), Did::Exit(130));
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
fn the_bound_kills_then_exits_only_once_five_seconds_pass() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    let until = clock.now() + BOUND;
    signals.handle(SIGTERM);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(-1));
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(BOUND.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(
        clock.await_parked(until, DEADLINE),
        "short of the bound it waits again"
    );
    assert!(calls.try_recv().is_err(), "nothing killed before the bound");
    assert!(did.try_recv().is_err(), "no exit before the bound");
    clock.advance(Duration::from_millis(1));
    assert_eq!(calls.recv_timeout(DEADLINE).unwrap(), Did::Bound);
    assert_eq!(did.recv_timeout(DEADLINE).unwrap(), Did::Exit(143));
}

#[test]
fn a_started_shutdown_has_the_same_bound() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (tx, calls) = mpsc::channel();
    arm(&signals, &tx);
    assert_eq!(start(&signals, &tx), None);
    let until = clock.now() + BOUND;
    signals.handle(SIGHUP);
    assert_eq!(calls.try_recv().unwrap(), Did::Signal(129));
    assert!(
        clock.await_parked(until, DEADLINE),
        "the bound waits on the clock"
    );
    clock.advance(BOUND);
    assert_eq!(calls.recv_timeout(DEADLINE).unwrap(), Did::Bound);
    assert_eq!(did.recv_timeout(DEADLINE).unwrap(), Did::Exit(129));
}

#[test]
fn a_first_signal_while_armed_runs_on_record_once_and_start_returns_its_code() {
    let clock = FakeClock::new();
    let (signals, did) = recorded(&clock);
    let (record_tx, record_calls) = mpsc::channel();
    let (bound_tx, bound_calls) = mpsc::channel();
    arm_with(&signals, &record_tx, &bound_tx);
    signals.handle(SIGTERM);
    signals.handle(SIGINT);
    assert_eq!(
        record_calls.try_recv().unwrap(),
        Did::Signal(-1),
        "on_record runs on the first signal"
    );
    assert!(
        record_calls.try_recv().is_err(),
        "on_record runs exactly once"
    );
    assert_eq!(start(&signals, &bound_tx), Some(143));
    assert!(did.try_recv().is_err(), "nothing exited before the bound");
    drop(bound_calls);
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

/// The child's marker: set, the test installs the signals and sends itself
/// SIGTERM while booting.
const CHILD: &str = "FIBER_DOORS_SIGNALS_CHILD";

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn a_real_sigterm_while_booting_exits_143() {
    if std::env::var_os(CHILD).is_some() {
        let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
        let _signals = Signals::install(clock).unwrap();
        fakes::kill_pid(std::process::id(), "TERM").unwrap();
        // The signals thread exits the process; this wait only bounds a
        // child whose exit never came.
        let (_held, never) = mpsc::channel::<()>();
        let _waited = never.recv_timeout(CHILD_DEADLINE);
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
    let Ok(status) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for the child to exit on SIGTERM");
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
    let mut signals = Signals::new(timed, exit);
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
