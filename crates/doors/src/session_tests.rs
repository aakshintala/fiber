//! Connections leaving, a full status-cache queue, and close waking a subscribe.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{ContextAdded, Empty, Event, ExtensionsLoaded, LoadedExtension, Notice};
use contract::inbox::Delivery;
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
    hold_mu: Mutex<()>,
    hold_cv: Condvar,
    parked: AtomicUsize,
    parked_mu: Mutex<()>,
    parked_cv: Condvar,
    writer_pause: Mutex<bool>,
    writer_cv: Condvar,
    writing: AtomicBool,
    write_mu: Mutex<()>,
    write_cv: Condvar,
    accept_waiting: AtomicBool,
    accept_mu: Mutex<()>,
    accept_cv: Condvar,
}

static CONTROL: Control = Control {
    pause: Mutex::new(false),
    pause_cv: Condvar::new(),
    cache_entered: AtomicUsize::new(0),
    flush_waiting: AtomicUsize::new(0),
    flush_mu: Mutex::new(()),
    flush_cv: Condvar::new(),
    hold_reader: AtomicBool::new(false),
    hold_mu: Mutex::new(()),
    hold_cv: Condvar::new(),
    parked: AtomicUsize::new(0),
    parked_mu: Mutex::new(()),
    parked_cv: Condvar::new(),
    writer_pause: Mutex::new(false),
    writer_cv: Condvar::new(),
    writing: AtomicBool::new(false),
    write_mu: Mutex::new(()),
    write_cv: Condvar::new(),
    accept_waiting: AtomicBool::new(false),
    accept_mu: Mutex::new(()),
    accept_cv: Condvar::new(),
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn before_cache_line(gate: &Gate) {
    let pause = lock(&CONTROL.pause);
    CONTROL.cache_entered.fetch_add(1, Ordering::Relaxed);
    CONTROL.pause_cv.notify_all();
    let (pause, _) = CONTROL
        .pause_cv
        .wait_timeout_while(pause, DEADLINE, |pause| *pause && !gate.stopped())
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        !*pause || gate.stopped(),
        "the status cache is still paused"
    );
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

fn release_writer() {
    *lock(&CONTROL.writer_pause) = false;
    CONTROL.writer_cv.notify_all();
}

fn pause_writer() {
    *lock(&CONTROL.writer_pause) = true;
}

fn wait_until_writer_blocks() {
    let guard = lock(&CONTROL.write_mu);
    let (_guard, _) = CONTROL
        .write_cv
        .wait_timeout_while(guard, DEADLINE, |_| {
            !CONTROL.writing.load(Ordering::Relaxed)
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        CONTROL.writing.load(Ordering::Relaxed),
        "the writer blocks on a client that is not reading"
    );
}

fn wait_until_accept_waits() {
    let guard = lock(&CONTROL.accept_mu);
    let (_guard, _) = CONTROL
        .accept_cv
        .wait_timeout_while(guard, DEADLINE, |_| {
            !CONTROL.accept_waiting.load(Ordering::Relaxed)
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        CONTROL.accept_waiting.load(Ordering::Relaxed),
        "accept waits after an error"
    );
}

pub(super) fn park_reader() {
    if !CONTROL.hold_reader.load(Ordering::Relaxed) {
        return;
    }
    {
        let _held = lock(&CONTROL.parked_mu);
        CONTROL.parked.fetch_add(1, Ordering::Relaxed);
        CONTROL.parked_cv.notify_all();
    }
    let hold = lock(&CONTROL.hold_mu);
    let (_hold, _) = CONTROL
        .hold_cv
        .wait_timeout_while(hold, DEADLINE, |_| {
            CONTROL.hold_reader.load(Ordering::Relaxed)
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        !CONTROL.hold_reader.load(Ordering::Relaxed),
        "the reader is still held at exit"
    );
}

pub(super) fn wait_if_writer_paused() {
    let paused = lock(&CONTROL.writer_pause);
    if !*paused {
        return;
    }
    let (paused, _) = CONTROL
        .writer_cv
        .wait_timeout_while(paused, DEADLINE, |paused| *paused)
        .unwrap_or_else(PoisonError::into_inner);
    assert!(!*paused, "the writer is still paused");
}

pub(super) fn note_writer_blocked<T>(body: impl FnOnce() -> T) -> T {
    if thread::current().name() != Some("writer") {
        return body();
    }
    {
        let _held = lock(&CONTROL.write_mu);
        CONTROL.writing.store(true, Ordering::Relaxed);
        CONTROL.write_cv.notify_all();
    }
    let result = body();
    {
        let _held = lock(&CONTROL.write_mu);
        CONTROL.writing.store(false, Ordering::Relaxed);
        CONTROL.write_cv.notify_all();
    }
    result
}

pub(super) fn note_accept_wait() {
    let _held = lock(&CONTROL.accept_mu);
    CONTROL.accept_waiting.store(true, Ordering::Relaxed);
    CONTROL.accept_cv.notify_all();
}

fn reset() {
    *lock(&CONTROL.pause) = false;
    CONTROL.cache_entered.store(0, Ordering::Relaxed);
    CONTROL.flush_waiting.store(0, Ordering::Relaxed);
    CONTROL.hold_reader.store(false, Ordering::Relaxed);
    CONTROL.hold_cv.notify_all();
    CONTROL.parked.store(0, Ordering::Relaxed);
    *lock(&CONTROL.writer_pause) = false;
    CONTROL.writer_cv.notify_all();
    CONTROL.writing.store(false, Ordering::Relaxed);
    CONTROL.accept_waiting.store(false, Ordering::Relaxed);
}

struct Release;

impl Drop for Release {
    fn drop(&mut self) {
        CONTROL.hold_reader.store(false, Ordering::Relaxed);
        CONTROL.hold_cv.notify_all();
        release_cache();
        release_writer();
    }
}

struct Opened {
    _temp: fakes::TempDir,
    clock: Arc<FakeClock>,
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
        clock,
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
        let parked = lock(&CONTROL.parked_mu);
        let (_parked, _) = CONTROL
            .parked_cv
            .wait_timeout_while(parked, DEADLINE, |_| {
                CONTROL.parked.load(Ordering::Relaxed) == 0
            })
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            CONTROL.parked.load(Ordering::Relaxed) > 0,
            "the reader reaches its exit while close joins it"
        );
    }
    assert!(
        done_rx.try_recv().is_err(),
        "close returns before the reader has finished"
    );
    {
        let _hold = lock(&CONTROL.hold_mu);
        CONTROL.hold_reader.store(false, Ordering::Relaxed);
        CONTROL.hold_cv.notify_all();
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

fn step() -> Event {
    Event::StepStarted(Empty {})
}

fn wide_context() -> Event {
    Event::ContextAdded(ContextAdded {
        text: "z".repeat(64 * 1024),
        extension: "e".into(),
        hook: "turn_start".into(),
    })
}

#[test]
fn a_disconnect_stops_a_writer_whose_queue_is_full() {
    reset();
    let temp = fakes::TempDir::new("fd");
    let sessions = temp.path().join("h/projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), timed).unwrap());
    let watcher = log.watch();
    let injector = watcher.injector();
    for _ in 0..1_200 {
        log.append(&notice(), None, None).unwrap();
    }
    crate::client::stop_writer(&injector, &id);
    let (mut peer, stream) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match peer.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        crate::client::write_loop(watcher, stream, false);
        if let Ok(()) = tx.send(()) {}
    });
    rx.recv_timeout(DEADLINE)
        .expect("the writer stops when its queue was full");
    drop(log);
}

#[test]
fn a_paused_writer_drops_ephemeral_lines_and_keeps_durable_ones() {
    reset();
    let _release = Release;
    pause_writer();
    let opened = open();
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "full");
            for _ in 0..1_200 {
                log.append(&notice(), None, None).unwrap();
            }
            for _ in 0..3 {
                log.append(&step(), None, None).unwrap();
            }
            release_writer();
            let mut notices = 0;
            let mut seqs = Vec::new();
            loop {
                let line = recv(&client);
                if line["kind"] == "notice" {
                    notices += 1;
                }
                if let Some(seq) = line["seq"].as_u64() {
                    seqs.push(seq);
                    if seq == 2 {
                        break;
                    }
                }
            }
            assert!(
                notices < 1_200,
                "a paused writer drops ephemeral lines, got {notices}"
            );
            assert_eq!(seqs, vec![0, 1, 2]);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_client_that_never_reads_does_not_hold_close_past_the_grace() {
    reset();
    let _release = Release;
    let opened = open();
    let clock = Arc::clone(&opened.clock);
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.slow(true);
            client
                .send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#)
                .unwrap();
            client
                .send(
                    r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
                )
                .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let wrote = Arc::new(AtomicUsize::new(0));
            let appender = {
                let log = Arc::clone(&log);
                let stop = Arc::clone(&stop);
                let wrote = Arc::clone(&wrote);
                thread::spawn(move || {
                    let line = wide_context();
                    let size = 64 * 1024;
                    while !stop.load(Ordering::Relaxed) {
                        let at = wrote.fetch_add(size, Ordering::Relaxed);
                        if at >= 64 * 1024 * 1024 {
                            return;
                        }
                        log.append(&line, None, None).unwrap();
                    }
                })
            };
            match inbox
                .recv_timeout(DEADLINE)
                .expect("the prompt is delivered")
            {
                Delivery::Prompt(_, ack) => drop(ack),
                Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Reply(..)
                | Delivery::Close(_) => panic!("the prompt is delivered"),
            }
            wait_until_writer_blocks();
            stop.store(true, Ordering::Relaxed);
            appender.join().unwrap();
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = rx.recv_timeout(DEADLINE).expect("the client is held");
    let until = clock.now() + Duration::from_secs(2);
    let (done_tx, done_rx) = mpsc::channel();
    let session = opened.session;
    let held = opened.log;
    thread::spawn(move || {
        session.close(held);
        if let Ok(()) = done_tx.send(()) {}
    });
    assert!(
        clock.await_parked(until, DEADLINE),
        "close waits for a writer that is not reading"
    );
    clock.advance(Duration::from_secs(2));
    done_rx
        .recv_timeout(DEADLINE)
        .expect("close returns once the grace has passed");
    drop(client);
}

#[test]
fn accept_waits_after_an_error_until_a_connection_ends() {
    reset();
    let opened = open();
    let gate = Arc::clone(&opened.session.gate);
    let waiter = Arc::clone(&gate);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        waiter.wait_for_room();
        if let Ok(()) = tx.send(()) {}
    });
    wait_until_accept_waits();
    assert!(
        rx.try_recv().is_err(),
        "accept waits until a connection ends or the session stops"
    );
    gate.end_writer();
    rx.recv_timeout(DEADLINE)
        .expect("a connection ending lets accept try again");
    close_within(opened.session, opened.log);
}

#[test]
fn an_interrupted_accept_does_not_wait_and_any_other_error_does() {
    assert!(super::accept_error_waits(io::ErrorKind::Other));
    assert!(!super::accept_error_waits(io::ErrorKind::Interrupted));
}

#[test]
fn an_equal_flush_token_is_not_newer() {
    assert!(!super::flush_token_is_newer(1, 1));
    assert!(super::flush_token_is_newer(2, 1));
    assert!(!super::flush_token_is_newer(1, 2));
}
