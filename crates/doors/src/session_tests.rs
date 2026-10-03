//! Connections leaving, a full status-cache queue, and close waking a subscribe.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, Thread};
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{Event, ExtensionsLoaded, LoadedExtension, Notice};
use fakes::Client;
use fakes::clock::FakeClock;
use log::Log;

use super::{Gate, Session};

const DEADLINE: Duration = Duration::from_secs(10);

struct Control {
    pause: Mutex<bool>,
    pause_cv: Condvar,
    cache_entered: AtomicUsize,
    flush_waiting: AtomicUsize,
    flush_mu: Mutex<()>,
    flush_cv: Condvar,
    hold_reader: AtomicBool,
    parked: Mutex<Vec<Thread>>,
    parked_cv: Condvar,
}

static CONTROL: Control = Control {
    pause: Mutex::new(false),
    pause_cv: Condvar::new(),
    cache_entered: AtomicUsize::new(0),
    flush_waiting: AtomicUsize::new(0),
    flush_mu: Mutex::new(()),
    flush_cv: Condvar::new(),
    hold_reader: AtomicBool::new(false),
    parked: Mutex::new(Vec::new()),
    parked_cv: Condvar::new(),
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn before_cache_line(gate: &Gate) {
    let mut pause = lock(&CONTROL.pause);
    CONTROL.cache_entered.fetch_add(1, Ordering::Relaxed);
    CONTROL.pause_cv.notify_all();
    while *pause && !gate.stopped() {
        pause = CONTROL
            .pause_cv
            .wait(pause)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

pub(super) fn note_flush_wait() {
    let _held = lock(&CONTROL.flush_mu);
    CONTROL.flush_waiting.fetch_add(1, Ordering::Relaxed);
    CONTROL.flush_cv.notify_all();
}

pub(super) fn release_cache() {
    *lock(&CONTROL.pause) = false;
    CONTROL.pause_cv.notify_all();
}

pub(super) fn park_reader() {
    if !CONTROL.hold_reader.load(Ordering::Relaxed) {
        return;
    }
    let current = thread::current();
    {
        let mut parked = lock(&CONTROL.parked);
        parked.push(current.clone());
        CONTROL.parked_cv.notify_all();
    }
    while CONTROL.hold_reader.load(Ordering::Relaxed) {
        thread::park();
    }
}

fn reset() {
    *lock(&CONTROL.pause) = false;
    CONTROL.cache_entered.store(0, Ordering::Relaxed);
    CONTROL.flush_waiting.store(0, Ordering::Relaxed);
    CONTROL.hold_reader.store(false, Ordering::Relaxed);
    lock(&CONTROL.parked).clear();
}

struct Release;

impl Drop for Release {
    fn drop(&mut self) {
        CONTROL.hold_reader.store(false, Ordering::Relaxed);
        for thread in lock(&CONTROL.parked).drain(..) {
            thread.unpark();
        }
        release_cache();
    }
}

struct Opened {
    _temp: fakes::TempDir,
    log: Arc<Log>,
    session: Session,
    socket: std::path::PathBuf,
}

fn open() -> Opened {
    let temp = fakes::TempDir::new("fd");
    let home = temp.path().join("h");
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let dir = sessions.join(&id.0);
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let session =
        Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())).unwrap();
    Opened {
        _temp: temp,
        log,
        session,
        socket: home.join("run").join(&id.0),
    }
}

fn close_within(session: Session, log: Arc<Log>) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = tx.send(()) {}
    });
    rx.recv_timeout(DEADLINE).expect("close returned");
}

fn notice() -> Event {
    Event::Notice(Notice {
        code: ErrorCode::IoFailed,
        message: "n".into(),
        extension: None,
    })
}

fn extensions() -> Event {
    Event::ExtensionsLoaded(ExtensionsLoaded {
        extensions: vec![LoadedExtension {
            name: "demo".into(),
            version: "1".into(),
        }],
    })
}

fn subscribe(client: &Client, id: &str, level: &str) {
    client
        .send(&format!(
            r#"{{"id":"{id}","command":"subscribe","args":{{"level":"{level}"}}}}"#
        ))
        .unwrap();
}

fn recv(client: &Client) -> serde_json::Value {
    client
        .recv(DEADLINE)
        .expect("a line arrived before the deadline")
}

fn wait_until_cache_pauses() {
    let pause = lock(&CONTROL.pause);
    let (_pause, _) = CONTROL
        .pause_cv
        .wait_timeout_while(pause, DEADLINE, |_| {
            CONTROL.cache_entered.load(Ordering::Relaxed) == 0
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        CONTROL.cache_entered.load(Ordering::Relaxed) > 0,
        "the status cache pauses before it records a line"
    );
}

fn wait_until_flush_waits() {
    let guard = lock(&CONTROL.flush_mu);
    let (_guard, _) = CONTROL
        .flush_cv
        .wait_timeout_while(guard, DEADLINE, |_| {
            CONTROL.flush_waiting.load(Ordering::Relaxed) == 0
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        CONTROL.flush_waiting.load(Ordering::Relaxed) > 0,
        "subscribe waits for the status cache"
    );
}

fn descriptors() -> usize {
    fs::read_dir("/dev/fd").unwrap().count()
}

fn wait_idle(gate: &Gate) {
    let conns = super::lock(&gate.conns);
    let (conns, _) = gate
        .writers
        .wait_timeout_while(conns, DEADLINE, |conns| {
            !conns.live.is_empty() || conns.writers_open > 0
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        conns.live.is_empty() && conns.writers_open == 0,
        "a disconnected client left a socket or a thread held"
    );
}

#[test]
fn many_connections_leave_nothing_held() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    opened
        .session
        .run(Vec::new(), move |_inbox| {
            let before = descriptors();
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_live", "full");
            let _ack = recv(&client);
            {
                let conns = super::lock(&gate.conns);
                let (conns, _) = gate
                    .writers
                    .wait_timeout_while(conns, DEADLINE, |conns| {
                        !conns.live.iter().any(|(_, live)| {
                            live.reader.is_some()
                                && live.writer.is_some()
                                && live.shutdown.is_some()
                        })
                    })
                    .unwrap_or_else(PoisonError::into_inner);
                assert!(
                    conns.live.iter().any(|(_, live)| {
                        live.reader.is_some() && live.writer.is_some() && live.shutdown.is_some()
                    }),
                    "a live connection keeps its threads and its shutdown socket"
                );
            }
            drop(client);
            for i in 0..40 {
                let client = Client::connect(&socket).unwrap();
                subscribe(&client, &format!("c_{i}"), "full");
                let _ack = recv(&client);
                drop(client);
            }
            wait_idle(&gate);
            assert_eq!(
                descriptors(),
                before,
                "descriptors return to the count from before the clients connected"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn close_joins_a_reader_that_is_still_connected() {
    reset();
    let _release = Release;
    let opened = open();
    let socket = opened.socket.clone();
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            let _ack = recv(&client);
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = rx.recv_timeout(DEADLINE).expect("the client subscribed");
    CONTROL.hold_reader.store(true, Ordering::Relaxed);
    let (done_tx, done_rx) = mpsc::channel();
    let session = opened.session;
    let log = opened.log;
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    {
        let parked = lock(&CONTROL.parked);
        let (parked, _) = CONTROL
            .parked_cv
            .wait_timeout_while(parked, DEADLINE, |parked| parked.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            !parked.is_empty(),
            "the reader reaches its exit while close joins it"
        );
    }
    assert!(
        done_rx.try_recv().is_err(),
        "close returns before the reader has finished"
    );
    CONTROL.hold_reader.store(false, Ordering::Relaxed);
    for thread in lock(&CONTROL.parked).drain(..) {
        thread.unpark();
    }
    done_rx
        .recv_timeout(DEADLINE)
        .expect("close returns after the reader finishes");
    drop(client);
}

#[test]
fn subscribe_waits_out_a_full_cache_queue_and_close_wakes_it() {
    reset();
    let _release = Release;
    let opened = open();
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    *lock(&CONTROL.pause) = true;
    opened
        .session
        .run(Vec::new(), move |_inbox| {
            log.append(&extensions(), None, None).unwrap();
            wait_until_cache_pauses();
            for _ in 0..1_200 {
                log.append(&notice(), None, None).unwrap();
            }
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sum", "summary");
            assert_eq!(recv(&client)["payload"]["command_id"], "c_sum");
            wait_until_flush_waits();
            release_cache();
            let line = recv(&client);
            assert_eq!(line["kind"], "extensions_loaded");
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn close_wakes_a_subscribe_waiting_on_the_cache() {
    reset();
    let _release = Release;
    let opened = open();
    let socket = opened.socket.clone();
    *lock(&CONTROL.pause) = true;
    opened
        .session
        .run(Vec::new(), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sum", "summary");
            assert_eq!(recv(&client)["payload"]["command_id"], "c_sum");
            wait_until_flush_waits();
            drop(client);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}
