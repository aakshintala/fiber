//! Connections leaving, a lagging connection, and close joining a reader.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{
    CommandAccepted, Empty, Event, ExtensionsLoaded, FiberExited, LoadedExtension, Notice,
    SessionState, SessionStatus,
};
use contract::inbox::Delivery;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Process, Tokens, Usage};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, ErrorCode};
use fakes::Client;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::{Map, Value};

use super::{Gate, Session};

const DEADLINE: Duration = Duration::from_secs(10);

/// How long a test waits for `close` to return once the grace has passed:
/// shorter than [`DEADLINE`], the blocked test writer's own wait, so that
/// wait ending cannot make `close` return.
const CLOSE_AFTER_GRACE: Duration = Duration::from_secs(2);

struct Control {
    hold_reader: AtomicBool,
    hold_mu: Mutex<()>,
    hold_cv: Condvar,
    parked: AtomicUsize,
    parked_mu: Mutex<()>,
    parked_cv: Condvar,
    accept_waiting: AtomicBool,
    accept_mu: Mutex<()>,
    accept_cv: Condvar,
}

static CONTROL: Control = Control {
    hold_reader: AtomicBool::new(false),
    hold_mu: Mutex::new(()),
    hold_cv: Condvar::new(),
    parked: AtomicUsize::new(0),
    parked_mu: Mutex::new(()),
    parked_cv: Condvar::new(),
    accept_waiting: AtomicBool::new(false),
    accept_mu: Mutex::new(()),
    accept_cv: Condvar::new(),
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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

pub(super) fn note_accept_wait() {
    let _held = lock(&CONTROL.accept_mu);
    CONTROL.accept_waiting.store(true, Ordering::Relaxed);
    CONTROL.accept_cv.notify_all();
}

fn reset() {
    CONTROL.hold_reader.store(false, Ordering::Relaxed);
    CONTROL.hold_cv.notify_all();
    CONTROL.parked.store(0, Ordering::Relaxed);
    CONTROL.accept_waiting.store(false, Ordering::Relaxed);
}

struct Release;

impl Drop for Release {
    fn drop(&mut self) {
        CONTROL.hold_reader.store(false, Ordering::Relaxed);
        CONTROL.hold_cv.notify_all();
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
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
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
                    "a live connection keeps its reader, its writer and its shutdown"
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
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
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
fn a_lagging_connection_does_not_hide_the_latest_from_a_subscriber() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            log.append(&status("stale"), None, None).unwrap();
            let watcher = log.watch();
            let held = Held::new(Hold::Wait);
            attach_held(&gate, &held, watcher);
            log.append(&notice(), None, None).unwrap();
            held.wait_blocked();
            for _ in 0..1_200 {
                log.append(&notice(), None, None).unwrap();
            }
            log.append(&status("newest"), None, None).unwrap();
            log.append(&extensions(), None, None).unwrap();
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sum", "summary");
            assert_eq!(recv(&client)["payload"]["command_id"], "c_sum");
            let status_line = recv(&client);
            assert_eq!(status_line["kind"], "session_status");
            assert_eq!(
                status_line["payload"]["name"], "newest",
                "a status emitted after a connection's queue is full still reaches a later subscriber"
            );
            let extensions_line = recv(&client);
            assert_eq!(extensions_line["kind"], "extensions_loaded");
            assert_eq!(
                extensions_line["payload"]["extensions"][0]["name"], "demo",
                "a summary subscriber is sent the latest extensions"
            );
            held.release();
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

fn step() -> Event {
    Event::StepStarted(Empty {})
}

enum Hold {
    Wait,
    Go,
    Fail,
}

struct HeldState {
    entered: bool,
    hold: Hold,
}

struct Held {
    mu: Mutex<HeldState>,
    cv: Condvar,
    buf: Mutex<Vec<u8>>,
    buf_cv: Condvar,
}

struct HeldWrite {
    held: Arc<Held>,
}

impl Held {
    fn new(hold: Hold) -> Arc<Self> {
        Arc::new(Self {
            mu: Mutex::new(HeldState {
                entered: false,
                hold,
            }),
            cv: Condvar::new(),
            buf: Mutex::new(Vec::new()),
            buf_cv: Condvar::new(),
        })
    }

    fn fail(&self) {
        lock(&self.mu).hold = Hold::Fail;
        self.cv.notify_all();
    }

    fn release(&self) {
        lock(&self.mu).hold = Hold::Go;
        self.cv.notify_all();
    }

    fn wait_blocked(&self) {
        let guard = lock(&self.mu);
        let (guard, _) = self
            .cv
            .wait_timeout_while(guard, DEADLINE, |state| !state.entered)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(guard.entered, "the writer is blocked in write");
    }

    fn wait_text(&self, ready: impl Fn(&str) -> bool, what: &str) -> String {
        let guard = lock(&self.buf);
        let (guard, _) = self
            .buf_cv
            .wait_timeout_while(guard, DEADLINE, |buf| !ready(&String::from_utf8_lossy(buf)))
            .unwrap_or_else(PoisonError::into_inner);
        let text = String::from_utf8_lossy(&guard).into_owned();
        assert!(ready(&text), "{what}");
        text
    }
}

impl Write for HeldWrite {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut state = lock(&self.held.mu);
        state.entered = true;
        self.held.cv.notify_all();
        let (state, waited) = self
            .held
            .cv
            .wait_timeout_while(state, DEADLINE, |state| matches!(state.hold, Hold::Wait))
            .unwrap_or_else(PoisonError::into_inner);
        if waited.timed_out() && matches!(state.hold, Hold::Wait) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the writer was not released",
            ));
        }
        match state.hold {
            Hold::Fail => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the connection shut down",
            )),
            Hold::Wait => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the writer was not released",
            )),
            Hold::Go => {
                drop(state);
                lock(&self.held.buf).extend_from_slice(data);
                self.held.buf_cv.notify_all();
                Ok(data.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn lines_of(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn attach_held(gate: &Arc<super::Gate>, held: &Arc<Held>, watcher: log::Watcher) {
    let (stop_tx, stop_rx) = mpsc::channel();
    let reader = thread::spawn(move || match stop_rx.recv_timeout(DEADLINE) {
        Ok(()) | Err(_) => {}
    });
    let fail = Arc::clone(held);
    let id = gate.push_reader(
        reader,
        Box::new(move || {
            fail.fail();
            if let Ok(()) = stop_tx.send(()) {}
        }),
    );
    crate::client::spawn_writer(
        Arc::clone(gate),
        id,
        watcher,
        Box::new(HeldWrite {
            held: Arc::clone(held),
        }),
        false,
    );
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 0,
        },
        cost: Some(0.0),
        subscription_cost: 0.0,
    }
}

fn exited() -> Event {
    Event::FiberExited(FiberExited {
        exit_code: 0,
        usage: usage(),
        final_message: None,
        error: None,
        suspended_on: None,
        questions: None,
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
        crate::client::write_loop(watcher, Box::new(stream), false);
        if let Ok(()) = tx.send(()) {}
    });
    rx.recv_timeout(DEADLINE)
        .expect("the writer stops when its queue was full");
    drop(log);
}

#[test]
fn an_acknowledgement_survives_a_lagged_queue() {
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
    log.append(&step(), None, None).unwrap();
    let ack = crate::session::envelope(
        &id,
        clock.as_ref(),
        &Event::CommandAccepted(CommandAccepted {
            command_id: CommandId("c_tools".into()),
            result: None,
        }),
    );
    injector.push_kept(ack);
    let held = Held::new(Hold::Go);
    let write = HeldWrite {
        held: Arc::clone(&held),
    };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        crate::client::write_loop(watcher, Box::new(write), false);
        if let Ok(()) = tx.send(()) {}
    });
    let text = held.wait_text(
        |text| text.contains("c_tools") && text.contains("step_started"),
        "the acknowledgement and the catch-up line arrive",
    );
    let ack_at = text
        .find("c_tools")
        .expect("the acknowledgement arrives from a lagged queue");
    let step_at = text
        .find("step_started")
        .expect("catch-up returns the durable line the queue dropped");
    assert!(
        ack_at < step_at,
        "a kept acknowledgement is returned before catch-up"
    );
    drop(log);
    rx.recv_timeout(DEADLINE)
        .expect("the writer ends once the log is dropped");
}

#[test]
fn a_blocked_writer_holds_close_until_the_grace_passes() {
    reset();
    let opened = open();
    let clock = Arc::clone(&opened.clock);
    let gate = Arc::clone(&opened.session.gate);
    let watcher = opened.log.watch();
    opened.log.append(&notice(), None, None).unwrap();
    let held = Held::new(Hold::Wait);
    attach_held(&gate, &held, watcher);
    held.wait_blocked();
    let until = clock.now() + super::GRACE;
    let (done_tx, done_rx) = mpsc::channel();
    let session = opened.session;
    let log = opened.log;
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    assert!(
        clock.await_parked(until, DEADLINE),
        "close waits for a writer that is blocked in write"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "close has not returned before the grace passes"
    );
    clock.advance(super::GRACE.saturating_sub(Duration::from_millis(1)));
    assert!(
        done_rx.try_recv().is_err(),
        "close has not returned one millisecond before the grace"
    );
    clock.advance(Duration::from_millis(1));
    done_rx
        .recv_timeout(CLOSE_AFTER_GRACE)
        .expect("close returns once the grace has passed");
}

#[test]
fn the_grace_ends_at_its_deadline() {
    let clock = FakeClock::new();
    let now = clock.now();
    assert!(super::grace_remains(now, now + Duration::from_millis(1)));
    assert!(!super::grace_remains(now, now));
}

#[test]
fn a_published_reader_is_shut_down_before_it_is_joined() {
    reset();
    let opened = open();
    let gate = Arc::clone(&opened.session.gate);
    let (peer, mut stream) = UnixStream::pair().unwrap();
    let shutdown = stream.try_clone().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        if let Ok(()) = started_tx.send(()) {}
        let mut buf = [0u8; 1];
        match stream.read(&mut buf) {
            Ok(_) | Err(_) => {}
        }
    });
    started_rx
        .recv_timeout(DEADLINE)
        .expect("the reader is running");
    let id = gate.push_reader(reader, crate::client::shutdown_both(shutdown));
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        gate.finish(id);
        if let Ok(()) = done_tx.send(()) {}
    });
    done_rx
        .recv_timeout(DEADLINE)
        .expect("reap shuts the reader down before joining it");
    drop(peer);
    close_within(opened.session, opened.log);
}

#[test]
fn a_reader_that_reaps_itself_finishes() {
    reset();
    let opened = open();
    let gate = Arc::clone(&opened.session.gate);
    let (peer, stream) = UnixStream::pair().unwrap();
    let shutdown = stream.try_clone().unwrap();
    let (id_tx, id_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let child = Arc::clone(&gate);
    let reader = thread::spawn(move || {
        let Ok(id) = id_rx.recv_timeout(DEADLINE) else {
            return;
        };
        crate::client::serve(stream, child, id);
        if let Ok(()) = done_tx.send(()) {}
    });
    let id = gate.push_reader(reader, crate::client::shutdown_both(shutdown));
    if let Ok(()) = id_tx.send(id) {}
    drop(peer);
    done_rx
        .recv_timeout(DEADLINE)
        .expect("a reader that reaps itself finishes");
    close_within(opened.session, opened.log);
}

#[test]
fn a_reading_writer_records_fiber_exited_before_it_ends() {
    reset();
    let opened = open();
    let gate = Arc::clone(&opened.session.gate);
    let watcher = opened.log.watch();
    opened.log.append(&exited(), None, None).unwrap();
    let held = Held::new(Hold::Go);
    attach_held(&gate, &held, watcher);
    held.wait_text(
        |text| text.contains("fiber_exited"),
        "a reading writer records fiber_exited",
    );
    {
        let conns = super::lock(&gate.conns);
        assert!(
            conns.writers_open > 0,
            "fiber_exited arrives before the writer ends"
        );
    }
    close_within(opened.session, opened.log);
}

#[test]
fn a_slow_writer_keeps_durable_lines_in_seq_order() {
    reset();
    let opened = open();
    let gate = Arc::clone(&opened.session.gate);
    let watcher = opened.log.watch();
    opened.log.append(&notice(), None, None).unwrap();
    let held = Held::new(Hold::Wait);
    attach_held(&gate, &held, watcher);
    held.wait_blocked();
    let log = Arc::clone(&opened.log);
    for _ in 0..1_200 {
        log.append(&notice(), None, None).unwrap();
    }
    for _ in 0..3 {
        log.append(&step(), None, None).unwrap();
    }
    drop(log);
    held.release();
    let text = held.wait_text(
        |text| lines_of(text).iter().any(|line| line["seq"] == 2),
        "the durable lines arrive after the writer is released",
    );
    let lines = lines_of(&text);
    let durable: Vec<&str> = lines
        .iter()
        .filter(|line| line["seq"].is_u64())
        .filter_map(|line| line["kind"].as_str())
        .collect();
    assert_eq!(
        durable,
        ["step_started", "step_started", "step_started"],
        "durable lines arrive in seq order"
    );
    let seqs: Vec<u64> = lines
        .iter()
        .filter_map(|line| line["seq"].as_u64())
        .collect();
    assert_eq!(seqs, vec![0, 1, 2]);
    let notices = lines.iter().filter(|line| line["kind"] == "notice").count();
    assert!(
        notices < 1_200,
        "a slow writer drops ephemeral lines, got {notices}"
    );
    close_within(opened.session, opened.log);
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

fn status(name: &str) -> Event {
    Event::SessionStatus(SessionStatus {
        name: name.to_owned(),
        workspace: "/w".into(),
        parent: None,
        model: "m".into(),
        state: SessionState::Idle,
        since: 0,
        git: None,
        context: None,
        spend: usage(),
        delegates: 0,
        jobs: 0,
    })
}

#[test]
fn a_full_subscribers_latest_status_survives_a_queue_saturated_after_registration() {
    reset();
    let temp = fakes::TempDir::new("fd");
    let sessions = temp.path().join("h/projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id, timed).unwrap());
    log.append(&status("latest"), None, None).unwrap();
    // Registration, as `subscribe` does it, then the queue saturates and lags
    // before the latest status is delivered.
    let watcher = log.watch_all().unwrap();
    let injector = watcher.injector();
    let latest = log.latest("session_status");
    for _ in 0..1_200 {
        log.append(&notice(), None, None).unwrap();
    }
    log.append(&step(), None, None).unwrap();
    crate::client::queue_latest(&injector, latest, None);
    let held = Held::new(Hold::Go);
    let write = HeldWrite {
        held: Arc::clone(&held),
    };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        crate::client::write_loop(watcher, Box::new(write), false);
        if let Ok(()) = tx.send(()) {}
    });
    let text = held.wait_text(
        |text| text.contains("step_started"),
        "catch-up returns the durable line the saturated queue dropped",
    );
    let lines = lines_of(&text);
    let status = lines
        .iter()
        .find(|line| line["kind"] == "session_status")
        .expect("the latest status reaches a subscriber whose queue saturated");
    assert_eq!(status["payload"]["name"], "latest");
    drop(log);
    rx.recv_timeout(DEADLINE)
        .expect("the writer ends once the log is dropped");
}

/// A home whose `run/<id>` socket path fits no platform: longer than either
/// `SOCKET_PATH_MAX`.
fn overlong_home(temp: &fakes::TempDir) -> std::path::PathBuf {
    temp.path().join("h".repeat(200))
}

#[test]
fn resume_keeps_the_session_directory_when_the_socket_cannot_bind() {
    let temp = fakes::TempDir::new("fd");
    let home = overlong_home(&temp);
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);

    let timed: Arc<dyn Clock> = clock;
    let error = match Session::resume(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())) {
        Ok(_) => panic!("a socket path past the limit binds"),
        Err(error) => error,
    };

    assert_eq!(error.code, ErrorCode::Usage);
    assert!(dir.join("events.jsonl").is_file());
    // Nothing was written: the log holds no lines.
    assert!(log::read(&dir).unwrap().is_empty());
}

#[test]
fn open_deletes_a_new_session_directory_when_the_socket_cannot_bind() {
    let temp = fakes::TempDir::new("fd");
    let home = overlong_home(&temp);
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);

    let timed: Arc<dyn Clock> = clock;
    let error = match Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())) {
        Ok(_) => panic!("a socket path past the limit binds"),
        Err(error) => error,
    };

    assert_eq!(error.code, ErrorCode::Usage);
    assert!(!dir.exists());
}

#[test]
fn resume_replaces_a_stale_socket_file() {
    let temp = fakes::TempDir::new("fd");
    let home = temp.path().join("h");
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);
    let socket = home.join("run").join(&id.0);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    fs::write(&socket, "stale").unwrap();

    let timed: Arc<dyn Clock> = clock;
    let session =
        Session::resume(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())).unwrap();

    UnixStream::connect(&socket).unwrap();
    close_within(session, log);
}

#[test]
fn close_after_resume_keeps_a_session_that_has_turns() {
    use contract::events::{InputItem, TurnStarted};
    use contract::shapes::{ContentPart, Origin, Sender};

    let temp = fakes::TempDir::new("fd");
    let home = temp.path().join("h");
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);
    log.append(
        &Event::TurnStarted(TurnStarted {
            input: vec![InputItem::Message {
                content: vec![ContentPart::Text { text: "one".into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: CommandId("c_1".into()),
                },
                changed_by: None,
            }],
        }),
        Some(contract::TurnId("t_1".into())),
        None,
    )
    .unwrap();

    let timed: Arc<dyn Clock> = clock;
    let session =
        Session::resume(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())).unwrap();
    close_within(session, log);

    let lines = log::read(&dir).unwrap();
    assert_eq!(
        lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["turn_started"]
    );
}

#[test]
fn a_clock_move_delivers_cancelled_and_nothing_before() {
    let opened = open();
    let clock = Arc::clone(&opened.clock);
    let (entered, entered_rx) = mpsc::channel();
    thread::spawn(move || {
        entered_rx
            .recv_timeout(DEADLINE)
            .expect("the receiver is blocked");
        clock.advance(Duration::from_millis(1));
    });
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "nothing arrives before the clock moves"
            );
            entered.send(()).unwrap();
            let delivery = inbox
                .recv_timeout(DEADLINE)
                .expect("a clock move wakes the inbox");
            assert!(matches!(delivery, Delivery::Cancelled));
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

/// Longer than [`DEADLINE`], so a shell that misses its cancel holds `close`
/// past the test's own wait.
const SHELL_LIMIT: Duration = Duration::from_secs(30);

struct LateFlag {
    ready: Mutex<bool>,
    cv: Condvar,
}

impl Wake for LateFlag {
    fn wake(&self) {
        *lock(&self.ready) = true;
        self.cv.notify_all();
    }
}

struct LateShell {
    saw: Arc<AtomicBool>,
    entered: Mutex<Option<mpsc::Sender<()>>>,
}

impl Tool for LateShell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".to_owned(),
            description: "test".to_owned(),
            input_schema: Value::Object(Map::new()),
            deferred: false,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Err(EffectsError::Tool("unused".into()))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        let flag = Arc::new(LateFlag {
            ready: Mutex::new(false),
            cv: Condvar::new(),
        });
        let wake: Arc<dyn Wake> = flag.clone();
        cancel.subscribe(Arc::downgrade(&wake));
        let cancelled = cancel.is_cancelled();
        self.saw.store(cancelled, Ordering::Relaxed);
        if let Some(sender) = self.entered.lock().expect("the entered lock").take() {
            sender.send(()).expect("the test is waiting");
        }
        if !cancelled {
            let guard = flag.ready.lock().expect("the flag lock");
            let _wait = flag
                .cv
                .wait_timeout_while(guard, SHELL_LIMIT, |_| !cancel.is_cancelled());
        }
        Output {
            content: vec![ContentPart::Text {
                text: "stopped".to_owned(),
            }],
            process: Some(Process {
                exit_code: None,
                signal: None,
                timed_out: false,
            }),
            ..Output::default()
        }
    }
}

#[test]
fn a_shell_registered_after_close_is_cancelled() {
    let opened = open();
    let clock = Arc::clone(&opened.clock);
    let gate = Arc::clone(&opened.session.gate);
    let socket = opened.socket.clone();
    let saw = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = mpsc::channel();
    opened.session.shell(Arc::new(LateShell {
        saw: Arc::clone(&saw),
        entered: Mutex::new(Some(entered_tx)),
    }));
    let (client_tx, client_rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_late", "full");
            let _ack = recv(&client);
            client_tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = client_rx
        .recv_timeout(DEADLINE)
        .expect("the client connected");

    let watcher = opened.log.watch();
    opened.log.append(&notice(), None, None).unwrap();
    let held = Held::new(Hold::Wait);
    attach_held(&gate, &held, watcher);
    held.wait_blocked();

    let until = clock.now() + super::GRACE;
    let (done_tx, done_rx) = mpsc::channel();
    let session = opened.session;
    let log = opened.log;
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    assert!(
        clock.await_parked(until, DEADLINE),
        "close is past the shell snapshot and waiting out the grace"
    );
    client
        .send(r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#)
        .unwrap();
    entered_rx
        .recv_timeout(DEADLINE)
        .expect("the shell registered after close");
    assert!(saw.load(Ordering::Relaxed), "the tool saw its cancel");
    clock.advance(super::GRACE);
    done_rx
        .recv_timeout(DEADLINE)
        .expect("close returned without the tool's timeout");
}
