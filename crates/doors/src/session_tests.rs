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
use std::os::unix::fs::PermissionsExt;
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

/// A point a test observes through [`Gate::probe`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Probe {
    /// `close`'s first wait for the driver shells has returned.
    FirstShellWaitDone,
    /// A wait for the driver shells is about to block on one still running.
    ShellsWaiting,
}

/// What [`Gate::probe`] calls at each [`Probe`] point.
pub(crate) type Prober = Arc<dyn Fn(Probe) + Send + Sync>;

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

/// Waits until no connection is live, no writer is open and the process
/// holds `before` descriptors. A connection leaves `live` before `reap` drops
/// its socket's last clone; `Gate::finish` notifies under the lock after that,
/// so the count, not an empty `live`, proves the descriptors are released.
fn wait_released(gate: &Gate, before: usize) {
    let conns = super::lock(&gate.conns);
    let (_conns, waited) = gate
        .writers
        .wait_timeout_while(conns, DEADLINE, |conns| {
            !conns.live.is_empty() || conns.writers_open > 0 || descriptors() != before
        })
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        !waited.timed_out(),
        "descriptors return to the count from before the clients connected"
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
            wait_released(&gate, before);
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
fn open_refuses_a_socket_a_live_session_holds() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    // The same id in another project: its own new directory, the shared
    // socket the live session owns.
    let id = contract::SessionId(socket.file_name().unwrap().to_string_lossy().into_owned());
    let home = opened._temp.path().join("h");
    let sessions = home.join("projects/q/sessions");
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);

    let timed: Arc<dyn Clock> = clock;
    let error = match Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())) {
        Ok(_) => panic!("a socket a live session holds binds"),
        Err(error) => error,
    };

    assert_eq!(error.code, ErrorCode::SessionHeld);
    // Nothing the live session owns was removed: its socket still
    // answers. Only the refused session's own directory is cleaned up.
    UnixStream::connect(&socket).unwrap();
    assert!(!dir.exists());
    close_within(opened.session, opened.log);
}

#[test]
fn open_leaves_a_live_socket_it_cannot_connect_to() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    // A live listener whose mode hides it: connect fails, but a session is
    // still behind the path, so `open` must fail and leave the path alone.
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o000)).unwrap();
    // The same id in another project: its own new directory, the hidden
    // socket the live session owns.
    let id = contract::SessionId(socket.file_name().unwrap().to_string_lossy().into_owned());
    let home = opened._temp.path().join("h");
    let sessions = home.join("projects/q/sessions");
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);

    let timed: Arc<dyn Clock> = clock;
    let error = match Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())) {
        Ok(_) => panic!("a socket a live session holds binds"),
        Err(error) => error,
    };
    // Restored before any assert that can fail, so cleanup still unlinks it.
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();

    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(
        error.message.contains(&socket.display().to_string()),
        "the failure names the socket path"
    );
    assert!(
        fs::symlink_metadata(&socket).is_ok(),
        "a connect error that may hide a live session leaves its path in place"
    );
    // The live session still answers, and only the refused open's own
    // directory is cleaned up.
    UnixStream::connect(&socket).unwrap();
    assert!(!dir.exists());
    close_within(opened.session, opened.log);
}

#[test]
fn open_leaves_a_symlink_to_a_live_socket_it_cannot_connect_to() {
    reset();
    let opened = open();
    let live = opened.socket.clone();
    fs::set_permissions(&live, fs::Permissions::from_mode(0o000)).unwrap();
    // Another session's path is a symlink to the hidden live socket: it is
    // no regular file, so nothing proves it stale.
    let home = opened._temp.path().join("h");
    let id = contract::SessionId(crate::mint("s_"));
    let link = home.join("run").join(&id.0);
    std::os::unix::fs::symlink(&live, &link).unwrap();
    let sessions = home.join("projects/q/sessions");
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);

    let timed: Arc<dyn Clock> = clock;
    let error = match Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())) {
        Ok(_) => panic!("a symlink to a hidden live socket was replaced"),
        Err(error) => error,
    };
    // Restored before any assert that can fail, so cleanup still unlinks it.
    fs::set_permissions(&live, fs::Permissions::from_mode(0o600)).unwrap();

    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(
        fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink()),
        "the symlink stays in place"
    );
    UnixStream::connect(&live).unwrap();
    fs::remove_file(&link).unwrap();
    close_within(opened.session, opened.log);
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
                    command_id: Some(CommandId("c_1".into())),
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
            hosted: None,
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

#[test]
fn run_sends_the_jobs_ends_to_the_loops_inbox() {
    use contract::jobs::{Jobs as _, Opening, Stop};
    let opened = open();
    let dir = fakes::TempDir::new("fd-jobs");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    opened.session.jobs(jobs.clone());
    let job = jobs
        .open(Opening {
            tool: "shell".into(),
            description: "npm test".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: None,
        })
        .unwrap();
    let id = job.started.job_id.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            drop(job.end);
            let delivery = inbox
                .recv_timeout(DEADLINE)
                .expect("the job's end reached the inbox");
            let Delivery::Job(notice) = delivery else {
                panic!("the end sent {delivery:?}");
            };
            assert_eq!(notice.completed.job_id, id);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

/// Hooks that hand the inbox `deliver_to` gives them to the test.
struct InboxHooks(Mutex<Option<mpsc::Sender<mpsc::Sender<Delivery>>>>);

impl contract::hook::Hooks for InboxHooks {
    fn after_tool(
        &self,
        _call: &contract::hook::AfterToolCall<'_>,
    ) -> contract::hook::AfterToolAnswer {
        contract::hook::AfterToolAnswer {
            outcome: contract::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn deliver_to(&self, inbox: mpsc::Sender<Delivery>) {
        if let Some(tx) = self.0.lock().unwrap().as_ref() {
            let _sent = tx.send(inbox);
        }
    }
}

#[test]
fn run_hands_the_hooks_the_loops_inbox() {
    let opened = open();
    let (given_tx, given_rx) = mpsc::channel();
    opened
        .session
        .hooks(Arc::new(InboxHooks(Mutex::new(Some(given_tx)))));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let given = given_rx
                .recv_timeout(DEADLINE)
                .expect("waited for the hooks to be handed the inbox");
            given.send(Delivery::Cancelled).unwrap();
            let delivery = inbox
                .recv_timeout(DEADLINE)
                .expect("waited for the hooks' send to reach the loop's inbox");
            assert!(
                matches!(delivery, Delivery::Cancelled),
                "the hooks' sender feeds the loop's inbox: {delivery:?}"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

/// A driver shell that reports its start, then its cancel, and returns only
/// once the test releases it.
struct HeldShell {
    entered: Mutex<Option<mpsc::Sender<()>>>,
    cancelled: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Tool for HeldShell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".to_owned(),
            description: "test".to_owned(),
            input_schema: Value::Object(Map::new()),
            deferred: false,
            hosted: None,
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
        if let Some(sender) = lock(&self.entered).take() {
            sender.send(()).expect("the test is waiting");
        }
        let guard = lock(&flag.ready);
        let _wait = flag
            .cv
            .wait_timeout_while(guard, SHELL_LIMIT, |_| !cancel.is_cancelled());
        if cancel.is_cancelled()
            && let Some(sender) = lock(&self.cancelled).take()
        {
            sender.send(()).expect("the test is waiting");
        }
        if let Some(release) = lock(&self.release).take() {
            let _released = release.recv_timeout(SHELL_LIMIT);
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

/// A session with a [`HeldShell`] running one driver shell on a `full`
/// client. Returns the client, and the receivers that hear the shell's
/// cancel, and the sender that releases it.
struct Running {
    opened: Opened,
    client: Client,
    cancelled: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

fn running_shell(check: impl FnOnce(&mpsc::Receiver<Delivery>, &Session) + Send) -> Running {
    let opened = open();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (cancelled_tx, cancelled) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    opened.session.shell(Arc::new(HeldShell {
        entered: Mutex::new(Some(entered_tx)),
        cancelled: Mutex::new(Some(cancelled_tx)),
        release: Mutex::new(Some(release_rx)),
    }));
    let socket = opened.socket.clone();
    let mut connected = None;
    let session = &opened.session;
    session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_full", "full");
            let _ack = recv(&client);
            client
                .send(r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#)
                .unwrap();
            entered_rx
                .recv_timeout(DEADLINE)
                .expect("the driver shell started");
            check(&inbox, session);
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    Running {
        opened,
        client: connected.expect("the client connected"),
        cancelled,
        release,
    }
}

#[test]
fn the_stopper_cancels_a_running_driver_shell_and_wakes_the_loop() {
    let running = running_shell(|inbox, session| {
        (session.stopper())();
        let delivery = inbox
            .recv_timeout(DEADLINE)
            .expect("the stopper wakes the inbox");
        assert!(matches!(delivery, Delivery::Cancelled));
    });
    running
        .cancelled
        .recv_timeout(DEADLINE)
        .expect("the driver shell saw its cancel");
    running.release.send(()).unwrap();
    drop(running.client);
    close_within(running.opened.session, running.opened.log);
}

#[test]
fn the_stopper_after_the_inbox_is_gone_still_cancels_later_shells() {
    let opened = open();
    (opened.session.stopper())();
    assert!(super::lock(&opened.session.gate.shells).stopped);
    close_within(opened.session, opened.log);
}

#[test]
fn abandon_unregisters_a_driver_shell_that_never_ran() {
    let opened = open();
    opened.session.shell(Arc::new(HeldShell {
        entered: Mutex::new(None),
        cancelled: Mutex::new(None),
        release: Mutex::new(None),
    }));
    let gate = Arc::clone(&opened.session.gate);
    let command = contract::commands::Shell {
        command: "true".to_owned(),
        send: false,
    };
    let Ok(running) = crate::shell::start(&gate, &command) else {
        panic!("the driver shell passed its checks");
    };
    assert_eq!(super::lock(&gate.shells).running.len(), 1);
    running.abandon(&gate);
    assert!(
        super::lock(&gate.shells).running.is_empty(),
        "close would wait for a shell that never runs"
    );
    close_within(opened.session, opened.log);
}

#[test]
fn quiesce_cancels_a_running_driver_shell_and_waits_for_it() {
    let running = running_shell(|_, _| {});
    let session = running.opened.session;
    let (done_tx, done) = mpsc::channel();
    let gate = Arc::clone(&session.gate);
    let quiescing = thread::spawn(move || {
        session.quiesce();
        done_tx.send(()).unwrap();
        session
    });
    running
        .cancelled
        .recv_timeout(DEADLINE)
        .expect("quiesce cancelled the driver shell");
    // The shell is cancelled but still running: quiesce is still waiting.
    assert!(!super::lock(&gate.shells).running.is_empty());
    assert!(
        done.try_recv().is_err(),
        "quiesce returned before the shell ended"
    );
    running.release.send(()).unwrap();
    done.recv_timeout(DEADLINE)
        .expect("quiesce returned once the shell ended");
    let session = quiescing.join().unwrap();
    assert!(super::lock(&gate.shells).running.is_empty());
    drop(running.client);
    close_within(session, running.opened.log);
}

#[test]
fn close_cancels_a_running_driver_shell_and_waits_for_its_answer() {
    let running = running_shell(|_, _| {});
    let session = running.opened.session;
    let log = running.opened.log;
    let (done_tx, done) = mpsc::channel();
    let gate = Arc::clone(&session.gate);
    thread::spawn(move || {
        session.close(log);
        done_tx.send(()).unwrap();
    });
    running
        .cancelled
        .recv_timeout(DEADLINE)
        .expect("close cancelled the driver shell");
    // The shell is cancelled but still running: close is still waiting,
    // before it stops accepting. A subscriber's writer would hold close
    // later anyway, as the shell's answer keeps its queue open.
    assert!(!super::lock(&gate.shells).running.is_empty());
    assert!(!gate.stopped(), "close went on before the shell ended");
    running.release.send(()).unwrap();
    // The answer is queued before the shell leaves the registry, so it
    // reaches the client before close drops the log.
    let answer = loop {
        let line = recv(&running.client);
        if line["payload"]["command_id"] == "c_shell" {
            break line;
        }
    };
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    assert_eq!(answer["payload"]["result"]["output"], "stopped");
    done.recv_timeout(DEADLINE)
        .expect("close returned once the shell ended");
    assert!(super::lock(&gate.shells).running.is_empty());
}

#[test]
fn a_driver_shell_started_after_the_stopper_is_cancelled_and_waited_for() {
    let opened = open();
    let (entered_tx, entered) = mpsc::channel();
    let (cancelled_tx, cancelled) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    opened.session.shell(Arc::new(HeldShell {
        entered: Mutex::new(Some(entered_tx)),
        cancelled: Mutex::new(Some(cancelled_tx)),
        release: Mutex::new(Some(release_rx)),
    }));
    let gate = Arc::clone(&opened.session.gate);
    let socket = opened.socket.clone();
    let mut connected = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_| {
            (opened.session.stopper())();
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_full", "full");
            let _ack = recv(&client);
            client
                .send(r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#)
                .unwrap();
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    entered
        .recv_timeout(DEADLINE)
        .expect("the driver shell started");
    cancelled
        .recv_timeout(DEADLINE)
        .expect("the driver shell started cancelled");
    // Registered although shutdown had begun, so close waits for it.
    assert!(!super::lock(&gate.shells).running.is_empty());
    release.send(()).unwrap();
    drop(connected);
    close_within(opened.session, opened.log);
    assert!(super::lock(&gate.shells).running.is_empty());
}

#[test]
fn close_waits_for_a_driver_shell_admitted_after_its_first_wait() {
    let opened = open();
    let (entered_tx, entered) = mpsc::channel();
    let (cancelled_tx, cancelled) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    opened.session.shell(Arc::new(HeldShell {
        entered: Mutex::new(Some(entered_tx)),
        cancelled: Mutex::new(Some(cancelled_tx)),
        release: Mutex::new(Some(release_rx)),
    }));
    let gate = Arc::clone(&opened.session.gate);
    let (first_tx, first) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel::<()>();
    let resume_rx = Mutex::new(resume_rx);
    let (waiting_tx, waiting) = mpsc::channel();
    *super::lock(&gate.probe) = Some(Arc::new(move |point| match point {
        Probe::FirstShellWaitDone => {
            if let Ok(()) = first_tx.send(()) {}
            lock(&resume_rx)
                .recv_timeout(DEADLINE)
                .expect("the test resumes close");
        }
        Probe::ShellsWaiting => if let Ok(()) = waiting_tx.send(()) {},
    }));
    let socket = opened.socket.clone();
    let mut connected = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_full", "full");
            let _ack = recv(&client);
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    let client = connected.expect("the client connected");
    let (done_tx, done) = mpsc::channel();
    let session = opened.session;
    let log = opened.log;
    thread::spawn(move || {
        session.close(log);
        if let Ok(()) = done_tx.send(()) {}
    });
    first
        .recv_timeout(DEADLINE)
        .expect("close's first wait saw no driver shell");
    client
        .send(r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#)
        .unwrap();
    entered
        .recv_timeout(DEADLINE)
        .expect("the reader admitted the driver shell");
    cancelled
        .recv_timeout(DEADLINE)
        .expect("a shell admitted during teardown starts cancelled");
    resume.send(()).unwrap();
    waiting
        .recv_timeout(DEADLINE)
        .expect("close waits again for the shell once no reader is left");
    assert!(
        done.try_recv().is_err(),
        "close returned while the shell ran"
    );
    release.send(()).unwrap();
    done.recv_timeout(DEADLINE)
        .expect("close returned once the shell ended");
    assert!(super::lock(&gate.shells).running.is_empty());
    drop(client);
}

#[test]
fn after_quiesce_a_client_leaving_writes_no_clients_line() {
    let opened = open();
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    let mut connected = None;
    let first = socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_| {
            let client = Client::connect(&first).unwrap();
            subscribe(&client, "c_full", "full");
            let _ack = recv(&client);
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    // The watcher blocks without a deadline, so its lines cross a channel
    // and the read below carries the deadline.
    let mut watcher = opened.log.watch();
    let (forward, lines) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(Some(line)) = watcher.recv() {
            if forward.send(line).is_err() {
                return;
            }
        }
    });
    opened.session.quiesce();
    drop(connected);
    wait_idle(&gate);
    // A client attaching after quiesce writes none either.
    let late = Client::connect(&socket).unwrap();
    subscribe(&late, "c_late", "full");
    let _ack = recv(&late);
    opened.log.append(&notice(), None, None).unwrap();
    loop {
        let line = lines.recv_timeout(DEADLINE).expect("the notice arrives");
        assert_ne!(line.kind, "clients", "a clients line followed quiesce");
        if line.kind == "notice" {
            break;
        }
    }
    drop(late);
    close_within(opened.session, opened.log);
}

#[test]
fn serve_with_a_prompt_delivers_exactly_that_prompt_and_no_close() {
    reset();
    let opened = open();
    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran);
    opened
        .session
        .serve(Some("hi".to_owned()), Arc::new(|| false), |inbox| {
            seen.store(true, Ordering::Relaxed);
            let delivery = inbox.recv_timeout(DEADLINE).expect("the prompt arrives");
            let Delivery::Prompt(message, _) = delivery else {
                panic!("the first delivery is the prompt, got {delivery:?}");
            };
            assert_eq!(message.content.len(), 1);
            let ContentPart::Text { text } = &message.content[0] else {
                panic!("the prompt is text");
            };
            assert_eq!(text, "hi");
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "serve queues no close after its prompt"
            );
            Ok(())
        })
        .unwrap();
    assert!(
        ran.load(Ordering::Relaxed),
        "serve ran the loop with the queued prompt"
    );
    close_within(opened.session, opened.log);
}

#[test]
fn serve_without_a_prompt_delivers_nothing_until_a_client_sends() {
    reset();
    let opened = open();
    opened
        .session
        .serve(None, Arc::new(|| false), |inbox| {
            assert!(
                matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "serve with no prompt delivers nothing on its own"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

/// The prompt history of the project [`open`] puts its session in.
fn history_file(opened: &Opened) -> std::path::PathBuf {
    let home = opened.socket.parent().unwrap().parent().unwrap();
    home.join("projects/p/history.jsonl")
}

fn history_lines(file: &std::path::Path) -> Vec<Value> {
    match fs::read_to_string(file) {
        Ok(text) => lines_of(&text),
        Err(error) => {
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            Vec::new()
        }
    }
}

fn send_text(client: &Client, id: &str, command: &str, text: &str) {
    client
        .send(&format!(
            r#"{{"id":"{id}","command":"{command}","args":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        ))
        .unwrap();
}

/// The acknowledgement of command `id`, skipping any other line.
fn answer_of(client: &Client, id: &str) -> Value {
    loop {
        let line = recv(client);
        let acknowledges = matches!(
            line["kind"].as_str(),
            Some("command_accepted" | "command_rejected")
        );
        if acknowledges && line["payload"]["command_id"] == id {
            return line;
        }
    }
}

/// The next delivery, which must be a prompt; returns its acknowledgement.
fn next_prompt(inbox: &mpsc::Receiver<Delivery>) -> contract::inbox::Ack {
    let delivery = inbox.recv_timeout(DEADLINE).expect("a delivery arrives");
    let Delivery::Prompt(_, ack) = delivery else {
        panic!("expected a prompt, got {delivery:?}");
    };
    ack
}

fn busy() -> contract::inbox::Rejection {
    contract::inbox::Rejection {
        code: ErrorCode::Busy,
        message: "busy".into(),
    }
}

#[test]
fn a_served_session_appends_each_accepted_prompt_in_order() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    let session_id = opened.session.gate.session_id.0.clone();
    let ts = contract::clock::wall_ms(opened.clock.wall());
    let watched = file.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "summary");
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_1", "prompt", "one");
            (next_prompt(&inbox).0)(Ok(None));
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            assert_eq!(
                history_lines(&watched).len(),
                1,
                "the line is written before the acceptance"
            );
            send_text(&client, "c_2", "prompt", "two");
            (next_prompt(&inbox).0)(Ok(None));
            assert_eq!(answer_of(&client, "c_2")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    let lines = history_lines(&file);
    let texts: Vec<&Value> = lines
        .iter()
        .map(|line| &line["content"][0]["text"])
        .collect();
    assert_eq!(texts, ["one", "two"]);
    for line in &lines {
        assert_eq!(line["ts"], ts);
        assert_eq!(line["session_id"], session_id.as_str());
        assert_eq!(line["content"][0]["type"], "text");
        assert_eq!(line.as_object().unwrap().len(), 3);
    }
    close_within(opened.session, opened.log);
}

#[test]
fn a_served_session_appends_no_rejected_dropped_steered_or_own_prompt() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    opened
        .session
        .serve(Some("own".into()), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Ok(None));
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "summary");
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_busy", "prompt", "rejected");
            (next_prompt(&inbox).0)(Err(busy()));
            assert_eq!(answer_of(&client, "c_busy")["kind"], "command_rejected");
            send_text(&client, "c_drop", "prompt", "dropped");
            drop(next_prompt(&inbox));
            let dropped = answer_of(&client, "c_drop");
            assert_eq!(dropped["kind"], "command_rejected");
            assert_eq!(dropped["payload"]["code"], "closing");
            send_text(&client, "c_steer", "steer", "steered");
            let Delivery::Steer(_, ack) = inbox.recv_timeout(DEADLINE).unwrap() else {
                panic!("the steer arrives");
            };
            (ack.0)(Ok(None));
            assert_eq!(answer_of(&client, "c_steer")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    assert_eq!(history_lines(&file), Vec::<Value>::new());
    close_within(opened.session, opened.log);
}

#[test]
fn an_ask_session_appends_no_prompt_even_one_a_client_sends() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    opened
        .session
        .ask("asked".into(), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Ok(None));
            let Delivery::Close(close) = inbox.recv_timeout(DEADLINE).unwrap() else {
                panic!("ask queues close after its prompt");
            };
            (close.0)(Ok(None));
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "summary");
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_1", "prompt", "attached");
            (next_prompt(&inbox).0)(Ok(None));
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    assert_eq!(history_lines(&file), Vec::<Value>::new());
    close_within(opened.session, opened.log);
}

fn send_bare(client: &Client, id: &str, command: &str) {
    client
        .send(&format!(r#"{{"id":"{id}","command":"{command}"}}"#))
        .unwrap();
}

#[test]
fn a_repeated_prompt_id_is_rejected_on_another_connection_before_dispatch() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |inbox| {
            let first = Client::connect(&socket).unwrap();
            subscribe(&first, "c_sub", "summary");
            let _ack = answer_of(&first, "c_sub");
            send_text(&first, "c_1", "prompt", "one");
            (next_prompt(&inbox).0)(Ok(None));
            assert_eq!(answer_of(&first, "c_1")["kind"], "command_accepted");
            drop(first);
            let second = Client::connect(&socket).unwrap();
            subscribe(&second, "c_sub2", "summary");
            let _ack = answer_of(&second, "c_sub2");
            send_text(&second, "c_1", "prompt", "again");
            let repeat = answer_of(&second, "c_1");
            assert_eq!(repeat["kind"], "command_rejected");
            assert_eq!(repeat["payload"]["code"], "duplicate_command");
            assert!(inbox.try_recv().is_err(), "the repeat was not dispatched");
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_repeated_answered_command_id_is_rejected() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "summary");
            let _ack = answer_of(&client, "c_sub");
            send_bare(&client, "c_t", "tools");
            assert_eq!(answer_of(&client, "c_t")["kind"], "command_accepted");
            send_bare(&client, "c_t", "tools");
            let repeat = answer_of(&client, "c_t");
            assert_eq!(repeat["payload"]["code"], "duplicate_command");
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_rejected_command_id_may_be_sent_again() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "summary");
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_r", "prompt", "one");
            (next_prompt(&inbox).0)(Err(busy()));
            assert_eq!(answer_of(&client, "c_r")["payload"]["code"], "busy");
            send_text(&client, "c_r", "prompt", "one");
            (next_prompt(&inbox).0)(Ok(None));
            assert_eq!(answer_of(&client, "c_r")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_repeated_subscribe_id_is_a_duplicate_not_an_invalid_argument() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "summary");
            let _ack = answer_of(&client, "c_sub");
            subscribe(&client, "c_sub", "summary");
            let repeat = answer_of(&client, "c_sub");
            assert_eq!(repeat["payload"]["code"], "duplicate_command");
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_prompt_id_sent_again_while_the_first_is_unanswered_is_not_applied_twice() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "summary");
            let _ack = answer_of(&client, "c_sub");
            send_text(&client, "c_1", "prompt", "one");
            let held = next_prompt(&inbox);
            send_text(&client, "c_1", "prompt", "one");
            let repeat = answer_of(&client, "c_1");
            assert_eq!(repeat["payload"]["code"], "duplicate_command");
            assert!(inbox.try_recv().is_err(), "the repeat was not dispatched");
            (held.0)(Ok(None));
            assert_eq!(answer_of(&client, "c_1")["kind"], "command_accepted");
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_malformed_line_does_not_free_an_accepted_id() {
    reset();
    let opened = open();
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            subscribe(&client, "c_sub", "summary");
            let _ack = answer_of(&client, "c_sub");
            send_bare(&client, "c_t", "tools");
            assert_eq!(answer_of(&client, "c_t")["kind"], "command_accepted");
            client
                .send(r#"{"id":"c_t","command":"tools","bogus":1}"#)
                .unwrap();
            assert_eq!(answer_of(&client, "c_t")["payload"]["code"], "malformed");
            send_bare(&client, "c_t", "tools");
            assert_eq!(
                answer_of(&client, "c_t")["payload"]["code"],
                "duplicate_command"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}
