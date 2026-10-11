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
use std::os::unix::net::{UnixListener, UnixStream};
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
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;
use serde_json::{Map, Value};

use super::{Gate, Session};

#[allow(
    clippy::duplicate_mod,
    reason = "each unit-test file includes the shared support itself"
)]
#[path = "../tests/support/mod.rs"]
mod support;

use support::{DEADLINE, Opened, close_within, subscribe_line};

/// How long a held writer stays blocked in `write`: far past the test's own
/// waits, so when close never releases it the test's deadline names the hang
/// instead of the writer giving up and letting close return early.
const WRITE_HOLD: Duration = Duration::from_secs(30);

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
    skip_shutdown: AtomicBool,
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
    skip_shutdown: AtomicBool::new(false),
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

pub(super) fn shutdown_skipped() -> bool {
    CONTROL.skip_shutdown.load(Ordering::Relaxed)
}

/// A point a test observes through [`Gate::probe`]..
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Probe {
    /// `close`'s first wait for the driver shells has returned.
    FirstShellWaitDone,
    /// `close` has queued its `STOP` for the printer.
    PrinterStopQueued,
    /// A wait for the driver shells is about to block on one still running.
    ShellsWaiting,
    /// A `full` subscribe has its watcher and seed queued, before the writer
    /// starts.
    SubscribeSeeded,
    /// A `full` subscribe's acknowledgement is written.
    SubscribeAcknowledged,
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
    CONTROL.skip_shutdown.store(false, Ordering::Relaxed);
}

struct Release;

impl Drop for Release {
    fn drop(&mut self) {
        CONTROL.hold_reader.store(false, Ordering::Relaxed);
        CONTROL.hold_cv.notify_all();
    }
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

fn recv(client: &Client) -> serde_json::Value {
    client
        .recv(DEADLINE)
        .expect("a line arrived before the deadline")
}

fn descriptors() -> usize {
    fs::read_dir("/dev/fd").unwrap().count()
}

fn wait_idle(gate: &Gate) {
    assert!(
        gate.conns.wait_while(DEADLINE, |state| {
            state.live_len() != 0 || state.writers_open() > 0
        }),
        "a disconnected client left a socket or a thread held"
    );
}

/// Waits until no connection is live, no writer is open and the process
/// holds `before` descriptors. A connection leaves `live` before `reap` drops
/// its socket's last clone; `Gate::finish` notifies under the lock after that,
/// so the count, not an empty `live`, proves the descriptors are released.
fn wait_released(gate: &Gate, before: usize) {
    assert!(
        gate.conns.wait_while(DEADLINE, |state| {
            state.live_len() != 0 || state.writers_open() > 0 || descriptors() != before
        }),
        "descriptors return to the count from before the clients connected"
    );
}

#[test]
fn many_connections_leave_nothing_held() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let before = descriptors();
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_live", "full")).unwrap();
            let _ack = recv(&client);
            {
                assert!(
                    gate.conns.wait_while(DEADLINE, |state| {
                        state.live_len() != 1 || state.writers_open() == 0
                    }),
                    "a live connection keeps its reader, its writer and its shutdown"
                );
            }
            drop(client);
            let socket = socket.clone();
            fakes::within("forty connections", DEADLINE, move || {
                for i in 0..40 {
                    let client = Client::connect(&socket).unwrap();
                    client
                        .send(&subscribe_line(&format!("c_{i}"), "full"))
                        .unwrap();
                    let _ack = recv(&client);
                    drop(client);
                }
            });
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
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "full")).unwrap();
            let _ack = recv(&client);
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client subscribed");
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
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("close returns after the reader finishes");
    drop(client);
}

#[test]
fn close_returns_while_a_silent_client_stays_open() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    let (tx, rx) = mpsc::channel();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the client connected");
    // The reader is published and blocked: it holds its reader and its
    // shutdown, and the client sends nothing from here on.
    {
        assert!(
            gate.conns
                .wait_while(DEADLINE, |state| state.live_len() != 1),
            "the silent client's reader is published and blocked"
        );
    }
    CONTROL.skip_shutdown.store(true, Ordering::Relaxed);
    close_within(opened.session, opened.log);
    drop(client);
}

#[test]
fn a_lagging_connection_does_not_hide_the_latest_from_a_subscriber() {
    reset();
    let opened = Opened::open(Vec::new());
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
            client.send(&subscribe_line("c_sum", "summary")).unwrap();
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
            .wait_timeout_while(state, WRITE_HOLD, |state| matches!(state.hold, Hold::Wait))
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
    // The reader holds until shutdown sends or drops the sender, so only
    // close ends it.
    let reader = thread::spawn(move || match stop_rx.recv() {
        Ok(()) | Err(_) => {}
    });
    let fail = Arc::clone(held);
    let id = gate
        .conns
        .push_reader(
            reader,
            Box::new(move || {
                fail.fail();
                if let Ok(()) = stop_tx.send(()) {}
            }),
        )
        .expect("the gate is running");
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
        crate::client::write_loop(watcher, Box::new(stream), false, mpsc::channel().1);
        if let Ok(()) = tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&rx)
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
        crate::client::write_loop(watcher, Box::new(write), false, mpsc::channel().1);
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
    Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the writer ends once the log is dropped");
}

#[test]
fn a_blocked_writer_holds_close_until_the_grace_passes() {
    reset();
    let opened = Opened::open(Vec::new());
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
    let mark = clock.advance_marked(super::GRACE.saturating_sub(Duration::from_millis(1)));
    assert!(
        clock.await_parked_since(&mark, Some(until), DEADLINE),
        "close waits again one millisecond short of the grace"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "close has not returned one millisecond before the grace"
    );
    clock.advance(Duration::from_millis(1));
    Deadline::after(DEADLINE)
        .recv(&done_rx)
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
fn wait_writers_returns_at_once_without_writers_and_at_the_grace() {
    let clock = FakeClock::new();
    let now = clock.now();
    let until = now + super::GRACE;
    let before = until
        .checked_sub(Duration::from_millis(1))
        .expect("the grace exceeds a millisecond");
    let after = until + Duration::from_millis(1);
    // No writer: the fast-fail path, on both sides of the deadline.
    assert!(!super::conns::should_wait(0, before, until));
    assert!(!super::conns::should_wait(0, until, until));
    assert!(!super::conns::should_wait(0, after, until));
    // An open writer waits only before the deadline; the deadline itself
    // is already past the grace.
    assert!(super::conns::should_wait(1, before, until));
    assert!(!super::conns::should_wait(1, until, until));
    assert!(!super::conns::should_wait(1, after, until));
    // The count's width changes nothing: open is open.
    assert!(super::conns::should_wait(u32::MAX, before, until));
    assert!(!super::conns::should_wait(u32::MAX, until, until));
}

#[test]
fn a_published_reader_is_shut_down_before_it_is_joined() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let (peer, mut stream) = UnixStream::pair().unwrap();
    let peer = Mutex::new(Some(peer));
    let shut = Arc::new(AtomicBool::new(false));
    let reader = thread::spawn(move || {
        let mut buf = [0u8; 1];
        let ended = stream.read(&mut buf);
        drop(ended);
    });
    // The shutdown ends the reader by EOF, so it works whether the reader
    // has reached `read` yet or not.
    let flag = Arc::clone(&shut);
    let id = gate
        .conns
        .push_reader(
            reader,
            Box::new(move || {
                flag.store(true, Ordering::SeqCst);
                drop(lock(&peer).take());
            }),
        )
        .expect("the gate is running");
    assert!(
        gate.conns.wait_while(DEADLINE, |state| !state.published(id)),
        "the reader is published under its id"
    );
    let (done_tx, done_rx) = mpsc::channel();
    let finishing = Arc::clone(&gate);
    thread::spawn(move || {
        finishing.conns.finish(id);
        if let Ok(()) = done_tx.send(()) {}
    });
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("reap shuts the reader down before joining it");
    assert!(shut.load(Ordering::SeqCst), "reap ran the shutdown");
    close_within(opened.session, opened.log);
}

#[test]
fn a_reader_that_reaps_itself_finishes() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let (peer, stream) = UnixStream::pair().unwrap();
    let shutdown = stream.try_clone().unwrap();
    let (read, stop) = ::support::stoppable::reader(stream).unwrap();
    let (id_tx, id_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let child = Arc::clone(&gate);
    let reader = thread::spawn(move || {
        let Ok(id) = Deadline::after(DEADLINE).recv(&id_rx) else {
            return;
        };
        crate::client::serve(read, child, id);
        if let Ok(()) = done_tx.send(()) {}
    });
    let id = gate
        .conns
        .push_reader(reader, crate::client::ender(shutdown, stop))
        .expect("the gate is running");
    if let Ok(()) = id_tx.send(id) {}
    drop(peer);
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("a reader that reaps itself finishes");
    close_within(opened.session, opened.log);
}

#[test]
fn a_reading_writer_records_fiber_exited_before_it_ends() {
    reset();
    let opened = Opened::open(Vec::new());
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
        assert!(
            gate.conns
                .wait_while(DEADLINE, |state| state.writers_open() == 0),
            "fiber_exited arrives before the writer ends"
        );
    }
    close_within(opened.session, opened.log);
}

#[test]
fn a_slow_writer_keeps_durable_lines_in_seq_order() {
    reset();
    let opened = Opened::open(Vec::new());
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
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    let waiter = Arc::clone(&gate);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        waiter.conns.wait_for_room();
        if let Ok(()) = tx.send(()) {}
    });
    wait_until_accept_waits();
    assert!(
        rx.try_recv().is_err(),
        "accept waits until a connection ends or the session stops"
    );
    gate.conns.end_writer();
    Deadline::after(DEADLINE)
        .recv(&rx)
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
        project: "-w".into(),
        clients: 0,
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
    let watcher = log.watch_all_seeded();
    for _ in 0..1_200 {
        log.append(&notice(), None, None).unwrap();
    }
    log.append(&step(), None, None).unwrap();
    let held = Held::new(Hold::Go);
    let write = HeldWrite {
        held: Arc::clone(&held),
    };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        crate::client::write_loop(watcher, Box::new(write), false, mpsc::channel().1);
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
    Deadline::after(DEADLINE)
        .recv(&rx)
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
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    // The same id in another project: its own new directory, the shared
    // socket the live session owns.
    let id = contract::SessionId(socket.file_name().unwrap().to_string_lossy().into_owned());
    let home = opened.home.clone();
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

/// Connecting to a socket whose mode is 0o000 fails with permission denied.
/// Root connects anyway, so a run as root fails here and says why instead of
/// failing later on an unrelated assertion.
fn assert_connect_denied(socket: &std::path::Path) {
    match UnixStream::connect(socket) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
        other => panic!(
            "these tests need a user other than root: connecting to a mode-0 socket gave {:?}",
            other.map(|_| ())
        ),
    }
}

#[test]
fn open_leaves_a_live_socket_it_cannot_connect_to() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    // A live listener whose mode hides it: connect fails, but a session is
    // still behind the path, so `open` must fail and leave the path alone.
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o000)).unwrap();
    assert_connect_denied(&socket);
    // The same id in another project: its own new directory, the hidden
    // socket the live session owns.
    let id = contract::SessionId(socket.file_name().unwrap().to_string_lossy().into_owned());
    let home = opened.home.clone();
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
    let opened = Opened::open(Vec::new());
    let live = opened.socket.clone();
    fs::set_permissions(&live, fs::Permissions::from_mode(0o000)).unwrap();
    assert_connect_denied(&live);
    // Another session's path is a symlink to the hidden live socket: it is
    // no regular file, so nothing proves it stale.
    let home = opened.home.clone();
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
    let opened = Opened::open(Vec::new());
    let clock = Arc::clone(&opened.clock);
    let (entered, entered_rx) = mpsc::channel();
    thread::spawn(move || {
        Deadline::after(DEADLINE)
            .recv(&entered_rx)
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
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
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
    let opened = Opened::open(Vec::new());
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
            client.send(&subscribe_line("c_late", "full")).unwrap();
            let _ack = recv(&client);
            client_tx.send(client).unwrap();
            Ok(())
        })
        .unwrap();
    let client = Deadline::after(DEADLINE)
        .recv(&client_rx)
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
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("the shell registered after close");
    assert!(saw.load(Ordering::Relaxed), "the tool saw its cancel");
    clock.advance(super::GRACE);
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("close returned without the tool's timeout");
}

#[test]
fn run_sends_the_jobs_ends_to_the_loops_inbox() {
    use contract::jobs::{Jobs as _, Opening, Stop};
    let opened = Opened::open(Vec::new());
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
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
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
    let opened = Opened::open(Vec::new());
    let (given_tx, given_rx) = mpsc::channel();
    opened
        .session
        .hooks(Arc::new(InboxHooks(Mutex::new(Some(given_tx)))));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let given = Deadline::after(DEADLINE)
                .recv(&given_rx)
                .expect("waited for the hooks to be handed the inbox");
            given.send(Delivery::Cancelled).unwrap();
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
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
        // The wait's guard is dropped before waiting for the test's
        // release below: the cancel path wakes through this same mutex,
        // so holding it across the release deadlocks the test inside
        // `ShellCancel::cancel` while this shell waits for the release.
        let (guard, _waited) = flag
            .cv
            .wait_timeout_while(guard, SHELL_LIMIT, |_| !cancel.is_cancelled())
            .unwrap_or_else(PoisonError::into_inner);
        drop(guard);
        if cancel.is_cancelled()
            && let Some(sender) = lock(&self.cancelled).take()
        {
            sender.send(()).expect("the test is waiting");
        }
        if let Some(release) = lock(&self.release).take() {
            let _released = release.recv();
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

/// Stops the session, then expects the loop's inbox to wake with
/// [`Delivery::Cancelled`]: what the stopper test checks mid-run. A check
/// returns what failed, and `running_shell` panics with it at the test's line.
fn stop_and_expect_cancel(
    inbox: &mpsc::Receiver<Delivery>,
    session: &Session,
) -> Result<(), &'static str> {
    (session.stopper())();
    match Deadline::after(DEADLINE).recv(inbox) {
        Ok(Delivery::Cancelled) => Ok(()),
        Ok(_) => Err("the stopper wakes the inbox with Cancelled"),
        Err(_) => Err("the stopper wakes the inbox"),
    }
}

/// No mid-run check: the shell runs until the test releases it.
fn no_check(_: &mpsc::Receiver<Delivery>, _: &Session) -> Result<(), &'static str> {
    Ok(())
}

#[track_caller]
fn running_shell(
    check: fn(&mpsc::Receiver<Delivery>, &Session) -> Result<(), &'static str>,
) -> Running {
    let opened = Opened::open(Vec::new());
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
    let mut failed = None;
    let session = &opened.session;
    session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            let _ack = recv(&client);
            client
                .send(r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#)
                .unwrap();
            failed = match Deadline::after(DEADLINE).recv(&entered_rx) {
                Ok(()) => check(&inbox, session).err(),
                Err(_) => Some("the driver shell started"),
            };
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    if let Some(what) = failed {
        panic!("{what}");
    }
    Running {
        opened,
        client: connected.expect("the client connected"),
        cancelled,
        release,
    }
}

#[test]
fn the_stopper_cancels_a_running_driver_shell_and_wakes_the_loop() {
    let running = running_shell(stop_and_expect_cancel);
    Deadline::after(DEADLINE)
        .recv(&running.cancelled)
        .expect("the driver shell saw its cancel");
    running.release.send(()).unwrap();
    drop(running.client);
    close_within(running.opened.session, running.opened.log);
}

#[test]
fn the_stopper_after_the_inbox_is_gone_still_cancels_later_shells() {
    let opened = Opened::open(Vec::new());
    (opened.session.stopper())();
    assert!(opened.session.gate.shells.sealed());
    close_within(opened.session, opened.log);
}

#[test]
fn abandon_unregisters_a_driver_shell_that_never_ran() {
    let opened = Opened::open(Vec::new());
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
    assert_eq!(gate.shells.running_len(), 1);
    running.abandon(&gate);
    assert!(
        gate.shells.running_len() == 0,
        "close would wait for a shell that never runs"
    );
    close_within(opened.session, opened.log);
}

#[test]
fn quiesce_cancels_a_running_driver_shell_and_waits_for_it() {
    let running = running_shell(no_check);
    let session = running.opened.session;
    let (done_tx, done) = mpsc::channel();
    let gate = Arc::clone(&session.gate);
    let quiescing = thread::spawn(move || {
        session.quiesce();
        done_tx.send(()).unwrap();
        session
    });
    Deadline::after(DEADLINE)
        .recv(&running.cancelled)
        .expect("quiesce cancelled the driver shell");
    // The shell is cancelled but still running: quiesce is still waiting.
    assert!(gate.shells.running_len() != 0);
    assert!(
        done.try_recv().is_err(),
        "quiesce returned before the shell ended"
    );
    running.release.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("quiesce returned once the shell ended");
    let session = quiescing.join().unwrap();
    assert_eq!(gate.shells.running_len(), 0);
    drop(running.client);
    close_within(session, running.opened.log);
}

#[test]
fn close_cancels_a_running_driver_shell_and_waits_for_its_answer() {
    let running = running_shell(no_check);
    let session = running.opened.session;
    let log = running.opened.log;
    let (done_tx, done) = mpsc::channel();
    let gate = Arc::clone(&session.gate);
    thread::spawn(move || {
        session.close(log);
        done_tx.send(()).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&running.cancelled)
        .expect("close cancelled the driver shell");
    // The shell is cancelled but still running: close is still waiting,
    // before it stops accepting. A subscriber's writer would hold close
    // later anyway, as the shell's answer keeps its queue open.
    assert!(gate.shells.running_len() != 0);
    assert!(
        !gate.conns.stopped(),
        "close went on before the shell ended"
    );
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
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returned once the shell ended");
    assert_eq!(gate.shells.running_len(), 0);
}

#[test]
fn a_driver_shell_started_after_the_stopper_is_cancelled_and_waited_for() {
    let opened = Opened::open(Vec::new());
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
            client.send(&subscribe_line("c_full", "full")).unwrap();
            let _ack = recv(&client);
            client
                .send(r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#)
                .unwrap();
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the driver shell started");
    Deadline::after(DEADLINE)
        .recv(&cancelled)
        .expect("the driver shell started cancelled");
    // Registered although shutdown had begun, so close waits for it.
    assert!(gate.shells.running_len() != 0);
    release.send(()).unwrap();
    drop(connected);
    close_within(opened.session, opened.log);
    assert_eq!(gate.shells.running_len(), 0);
}

#[test]
fn close_waits_for_a_driver_shell_admitted_after_its_first_wait() {
    let opened = Opened::open(Vec::new());
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
            lock(&resume_rx).recv().expect("the test resumes close");
        }
        Probe::ShellsWaiting => if let Ok(()) = waiting_tx.send(()) {},
        Probe::SubscribeSeeded | Probe::SubscribeAcknowledged | Probe::PrinterStopQueued => {}
    }));
    let socket = opened.socket.clone();
    let mut connected = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
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
    Deadline::after(DEADLINE)
        .recv(&first)
        .expect("close's first wait saw no driver shell");
    client
        .send(r#"{"id":"c_shell","command":"shell","args":{"command":"sleep 60"}}"#)
        .unwrap();
    Deadline::after(DEADLINE)
        .recv(&entered)
        .expect("the reader admitted the driver shell");
    Deadline::after(DEADLINE)
        .recv(&cancelled)
        .expect("a shell admitted during teardown starts cancelled");
    resume.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&waiting)
        .expect("close waits again for the shell once no reader is left");
    assert!(
        done.try_recv().is_err(),
        "close returned while the shell ran"
    );
    release.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("close returned once the shell ended");
    assert_eq!(gate.shells.running_len(), 0);
    drop(client);
}

#[test]
fn after_quiesce_a_client_leaving_writes_no_clients_line() {
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let gate = Arc::clone(&opened.session.gate);
    // The watcher blocks without a deadline, so its lines cross a channel
    // and the reads below carry the deadline.
    let mut watcher = opened.log.watch();
    let (forward, lines) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(Some(line)) = watcher.recv() {
            if forward.send(line).is_err() {
                return;
            }
        }
    });
    let mut connected = None;
    let first = socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_| {
            let client = Client::connect(&first).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            let _ack = recv(&client);
            connected = Some(client);
            Ok(())
        })
        .unwrap();
    // The ack precedes the connection's own `clients` line, so the test
    // reads that line before quiesce: one still unwritten would be taken
    // for a line that followed quiesce.
    let lines = fakes::within("the joining client's clients line", DEADLINE, move || {
        let wait = Deadline::after(DEADLINE);
        loop {
            let line = wait
                .recv(&lines)
                .expect("the joining client's clients line arrives");
            if line.kind == "clients" {
                break;
            }
        }
        lines
    });
    opened.session.quiesce();
    drop(connected);
    wait_idle(&gate);
    // A client attaching after quiesce writes none either.
    let late = Client::connect(&socket).unwrap();
    late.send(&subscribe_line("c_late", "full")).unwrap();
    let _ack = recv(&late);
    opened.log.append(&notice(), None, None).unwrap();
    let _lines = fakes::within("the notice", DEADLINE, move || {
        let wait = Deadline::after(DEADLINE);
        loop {
            let line = wait.recv(&lines).expect("the notice arrives");
            assert_ne!(line.kind, "clients", "a clients line followed quiesce");
            if line.kind == "notice" {
                break;
            }
        }
        lines
    });
    drop(late);
    close_within(opened.session, opened.log);
}

#[test]
fn serve_with_a_prompt_delivers_exactly_that_prompt_and_no_close() {
    reset();
    let opened = Opened::open(Vec::new());
    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&ran);
    opened
        .session
        .serve(Some("hi".to_owned()), Arc::new(|| false), |inbox| {
            seen.store(true, Ordering::Relaxed);
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the prompt arrives");
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
    let opened = Opened::open(Vec::new());
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
    let owned = id.to_owned();
    client
        .recv_until(DEADLINE, |line| {
            matches!(
                line["kind"].as_str(),
                Some("command_accepted" | "command_rejected")
            ) && line["payload"]["command_id"] == owned
        })
        .expect("the acknowledgement arrived before the deadline")
}

/// The next delivery, which must be a prompt; returns its acknowledgement.
#[track_caller]
fn next_prompt(inbox: &mpsc::Receiver<Delivery>) -> contract::inbox::Ack {
    let delivery = Deadline::after(DEADLINE)
        .recv(inbox)
        .expect("a delivery arrives");
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
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    let session_id = opened.session.gate.session_id.0.clone();
    let ts = contract::clock::wall_ms(opened.clock.wall());
    let watched = file.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
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
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    opened
        .session
        .serve(Some("own".into()), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Ok(None));
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
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
            let Delivery::Steer(_, ack) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
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
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    let file = history_file(&opened);
    opened
        .session
        .ask("asked".into(), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Ok(None));
            let Delivery::Close(close) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
                panic!("ask queues close after its prompt");
            };
            (close.0)(Ok(None));
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
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

fn ui_status(extension: &str, status: &str) -> Event {
    Event::ExtensionUi(contract::events::ExtensionUi {
        extension: extension.to_owned(),
        ui: contract::events::Ui::Status {
            status: status.to_owned(),
        },
    })
}

#[test]
fn a_full_subscriber_sees_the_kept_ui_line_before_a_later_one() {
    // The seed and the registration sit under one lock: emit status A,
    // connect a client whose subscribe parks at the seed probe, append
    // status B through the log from the test thread, then release the probe
    // and read the client's first lines. Required: A then B. On the old path
    // (register, read `latest`, then inject) the same probe sits between the
    // snapshot read and the injection, so B is queued live before A is
    // injected and the client reads B then A.
    reset();
    let opened = Opened::open(Vec::new());
    opened
        .log
        .append(&ui_status("fiber.test/a", "A"), None, None)
        .unwrap();
    let gate = Arc::clone(&opened.session.gate);
    let (parked_tx, parked) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    *super::lock(&gate.probe) = Some(Arc::new(move |point| {
        if point != Probe::SubscribeSeeded {
            return;
        }
        if let Ok(()) = parked_tx.send(()) {}
        Deadline::start().recv_or_fail(&lock(&release_rx), "the test releases the subscribe");
    }));
    let socket = opened.socket.clone();
    let log = Arc::clone(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            // The reader parks at the probe; it holds no test assertion yet.
            Deadline::after(DEADLINE)
                .recv(&parked)
                .expect("subscribe parked at its seed");
            // Appended after the watcher is registered: `append` returns, so no
            // lock is held when the probe releases.
            log.append(&ui_status("fiber.test/a", "B"), None, None)
                .unwrap();
            release_tx.send(()).unwrap();
            let ack = recv(&client);
            assert_eq!(ack["kind"], "command_accepted");
            let first = client
                .recv_until(DEADLINE, |line| line["kind"] == "extension_ui")
                .expect("the first extension_ui arrives");
            let second = client
                .recv_until(DEADLINE, |line| line["kind"] == "extension_ui")
                .expect("the second extension_ui arrives");
            assert_eq!(
                [
                    first["payload"]["status"].as_str().unwrap(),
                    second["payload"]["status"].as_str().unwrap()
                ],
                ["A", "B"]
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn a_full_subscribe_is_counted_before_its_acknowledgement() {
    // A client that reads its acknowledgement is counted in `clients`, so a
    // prompt sent once the acknowledgement arrives finds it connected.
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    // Held weakly: the gate keeps the probe, and `close` must drop the log's
    // last handle to end the writer.
    let log = Arc::downgrade(&opened.log);
    let (count_tx, count) = mpsc::channel();
    *super::lock(&gate.probe) = Some(Arc::new(move |point| {
        if point != Probe::SubscribeAcknowledged {
            return;
        }
        let latest = log
            .upgrade()
            .expect("the log is open")
            .latest("clients")
            .map_or(0, |line| line.payload["count"].as_u64().unwrap());
        if let Ok(()) = count_tx.send(latest) {}
    }));
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_full", "full")).unwrap();
            let counted = Deadline::after(DEADLINE)
                .recv(&count)
                .expect("the subscribe was acknowledged");
            assert_eq!(counted, 1, "the acknowledgement preceded the count");
            let ack = recv(&client);
            assert_eq!(ack["kind"], "command_accepted");
            assert_eq!(ack["payload"]["command_id"], "c_full");
            let clients = client
                .recv_until(DEADLINE, |line| line["kind"] == "clients")
                .expect("the clients line follows the seed");
            assert_eq!(clients["payload"]["count"], 1);
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

fn send_bare(client: &Client, id: &str, command: &str) {
    client
        .send(&format!(r#"{{"id":"{id}","command":"{command}"}}"#))
        .unwrap();
}

#[test]
fn a_rejected_command_id_may_be_sent_again() {
    reset();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
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
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
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
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
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
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .serve(None, Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
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

/// A resumed session whose log holds `events`, closed; whether its
/// directory is left.
#[track_caller]
fn kept_after_close(events: Vec<Event>) -> bool {
    let temp = fakes::TempDir::new("fd");
    let home = temp.path().join("h");
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = clock;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let dir = sessions.join(&id.0);
    for event in events {
        log.append(&event, None, None).unwrap();
    }
    let session =
        Session::resume(&home, &dir, &log, timed, Vec::new(), Box::new(io::sink())).unwrap();
    close_within(session, log);
    dir.exists()
}

#[test]
fn close_keeps_a_session_whose_log_holds_an_offer_and_no_turn() {
    use contract::events::{OfferedItem, OfferedKind, RepositoryCodeOffered};

    let offered = Event::RepositoryCodeOffered(RepositoryCodeOffered {
        request_id: contract::RequestId("r_1".into()),
        items: vec![OfferedItem {
            kind: OfferedKind::McpServer,
            name: "db".into(),
            hash: "0".repeat(64),
            required: false,
            summary: "MCP server: db".into(),
            version: None,
            diff: None,
        }],
    });
    assert!(kept_after_close(vec![offered]));
    assert!(!kept_after_close(Vec::new()));
}

fn tool_info(name: &str, bytes: u64) -> contract::events::ToolInfo {
    contract::events::ToolInfo {
        name: name.into(),
        source: contract::events::ToolSource::Builtin,
        state: contract::events::ToolState::Full,
        bytes,
        tokens: None,
    }
}

/// The `tools` answer's names and byte sizes, in answer order.
fn tools_answer(client: &Client, id: &str) -> Vec<(String, u64)> {
    send_bare(client, id, "tools");
    let answer = answer_of(client, id);
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    answer["payload"]["result"]["tools"]
        .as_array()
        .expect("a tools list")
        .iter()
        .map(|tool| {
            (
                tool["name"].as_str().unwrap().to_owned(),
                tool["bytes"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn named(entries: &[(&str, u64)]) -> Vec<(String, u64)> {
    entries
        .iter()
        .map(|(name, bytes)| ((*name).to_owned(), *bytes))
        .collect()
}

#[test]
fn the_declarer_changes_the_tools_answer() {
    reset();
    let opened = Opened::open(vec![tool_info("edit", 1), tool_info("write", 2)]);
    let socket = opened.socket.clone();
    let declare = opened.session.declarer();
    opened
        .session
        .serve(None, Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            client.send(&subscribe_line("c_sub", "summary")).unwrap();
            let _ack = answer_of(&client, "c_sub");
            declare("web_search", Some(tool_info("web_search", 52)));
            assert_eq!(
                tools_answer(&client, "c_1"),
                named(&[("edit", 1), ("web_search", 52), ("write", 2)]),
                "added in name order"
            );
            declare("web_search", Some(tool_info("web_search", 60)));
            assert_eq!(
                tools_answer(&client, "c_2"),
                named(&[("edit", 1), ("web_search", 60), ("write", 2)]),
                "replaced, not added twice"
            );
            declare("web_search", None);
            assert_eq!(
                tools_answer(&client, "c_3"),
                named(&[("edit", 1), ("write", 2)]),
                "removed"
            );
            declare("absent", None);
            assert_eq!(
                tools_answer(&client, "c_4"),
                named(&[("edit", 1), ("write", 2)])
            );
            declare("zz", Some(tool_info("zz", 3)));
            assert_eq!(
                tools_answer(&client, "c_5"),
                named(&[("edit", 1), ("write", 2), ("zz", 3)]),
                "a name after every other goes last"
            );
            declare("aaa", Some(tool_info("aaa", 4)));
            assert_eq!(
                tools_answer(&client, "c_6"),
                named(&[("aaa", 4), ("edit", 1), ("write", 2), ("zz", 3)]),
                "a name before every other goes first"
            );
            declare("aaa", None);
            assert_eq!(
                tools_answer(&client, "c_7"),
                named(&[("edit", 1), ("write", 2), ("zz", 3)]),
                "removes the first entry"
            );
            Ok(())
        })
        .unwrap();
    close_within(opened.session, opened.log);
}

#[test]
fn the_declarer_does_nothing_once_the_session_closes() {
    reset();
    let opened = Opened::open(vec![tool_info("edit", 1)]);
    let declare = opened.session.declarer();
    let gate = Arc::clone(&opened.session.gate);
    close_within(opened.session, opened.log);
    declare("web_search", Some(tool_info("web_search", 52)));
    declare("edit", None);
    assert_eq!(
        lock(&gate.tools)
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>(),
        ["edit"]
    );
    drop(gate);
    // With the gate gone, a call still neither panics nor changes anything.
    declare("web_search", None);
}

#[test]
fn the_inbox_wake_wakes_the_loop_and_never_keeps_the_inbox_open() {
    let opened = Opened::open(Vec::new());
    let wake = opened.session.inbox_wake();
    let mut kept = None;
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |inbox| {
            wake.wake();
            let delivery = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the wake reaches the inbox");
            assert!(matches!(delivery, Delivery::Cancelled));
            kept = Some(inbox);
            Ok(())
        })
        .unwrap();
    let inbox = kept.unwrap();
    close_within(opened.session, opened.log);
    // The session is closed: waking does nothing, and the wake alone does
    // not hold the inbox open.
    wake.wake();
    assert!(
        matches!(
            Deadline::after(DEADLINE).recv(&inbox),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ),
        "the inbox closes with the session"
    );
}

#[test]
fn close_keeps_a_started_rewound_session_with_no_prompt() {
    use contract::events::{FiberStarted, Rewind, SessionStarted, Variables, VariablesSource};
    use contract::shapes::Point;

    let started = || {
        Event::SessionStarted(SessionStarted {
            workspace: "/w".into(),
            variables: Variables {
                path: "/usr/bin".into(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: Some(Point {
                session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".into()),
                seq: contract::Seq(3),
            }),
            rewind: Some(Rewind {
                summary: None,
                note: "n".into(),
                jobs: Vec::new(),
            }),
            worktree: None,
        })
    };
    let fiber = || {
        Event::FiberStarted(FiberStarted {
            version: "0.0.0".into(),
            resumed: false,
        })
    };
    assert!(
        kept_after_close(vec![started(), fiber()]),
        "a started rewound session is kept"
    );
    assert!(
        !kept_after_close(vec![started()]),
        "a rewind that never started leaves nothing behind"
    );
    let plain = || {
        let Event::SessionStarted(mut first) = started() else {
            panic!("a session_started");
        };
        first.forked_from = None;
        first.rewind = None;
        Event::SessionStarted(first)
    };
    assert!(
        !kept_after_close(vec![plain(), fiber()]),
        "a started session with no point to continue from is deleted"
    );
}

#[test]
fn print_ends_at_rewound_while_the_log_is_held() {
    use contract::events::{Rewound, SessionStarted, Variables, VariablesSource};
    use contract::shapes::Point;

    struct SharedOut(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedOut {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            lock(&self.0).extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let temp = fakes::TempDir::new("fd");
    let sessions = temp.path().join("h").join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed: Arc<dyn Clock> = clock;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    let watcher = log.watch();
    // Seeded after the watch starts, as the printer sees them live.
    for event in [
        Event::SessionStarted(SessionStarted {
            workspace: "/w".into(),
            variables: Variables {
                path: "/usr/bin".into(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: Some(Point {
                session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".into()),
                seq: contract::Seq(3),
            }),
            rewind: None,
            worktree: None,
        }),
        Event::Rewound(Rewound {
            new_session_id: contract::SessionId("s_bbbbbbbbbbbbbbbb".into()),
            seq: contract::Seq(5),
            from_session_id: None,
            jobs: Vec::new(),
        }),
    ] {
        log.append(&event, None, None).unwrap();
    }
    let out = Arc::new(Mutex::new(Vec::new()));
    let (done, finished) = mpsc::channel();
    thread::Builder::new()
        .name("print-rewound".to_owned())
        .spawn({
            let out = Arc::clone(&out);
            move || {
                super::print(watcher, Box::new(SharedOut(out)));
                done.send(()).unwrap_or(());
            }
        })
        .unwrap();
    // The log stays held: without `rewound` closing the copy, the print
    // would wait for a `fiber_exited` that never comes.
    let _held = log;
    assert!(
        Deadline::after(DEADLINE).recv(&finished).is_ok(),
        "the copy ends at `rewound`"
    );
    let out = String::from_utf8_lossy(&lock(&out).clone()).into_owned();
    assert_eq!(out.lines().count(), 2, "both lines are copied");
    assert!(
        out.contains("\"rewound\""),
        "the last line copied is `rewound`"
    );
}

#[test]
fn ask_returns_the_first_prompts_rejection_as_its_failure() {
    reset();
    let opened = Opened::open(Vec::new());
    let failure = opened
        .session
        .ask("asked".into(), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Err(contract::inbox::Rejection {
                code: ErrorCode::InvalidArguments,
                message: "The MCP server `fx`'s prompt `/greet` needs <who>.".into(),
            }));
            let Delivery::Close(close) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
                panic!("ask queues close after its prompt");
            };
            (close.0)(Ok(None));
            Ok(())
        })
        .expect_err("the first prompt's rejection fails the ask");
    assert_eq!(failure.code, ErrorCode::InvalidArguments);
    assert_eq!(
        failure.message,
        "The MCP server `fx`'s prompt `/greet` needs <who>.",
    );
    assert_eq!(failure.retry_after_ms, None);
    assert_eq!(failure.provider, None);
    close_within(opened.session, opened.log);
}

#[test]
fn ask_whose_first_prompt_is_accepted_returns_what_run_returned() {
    reset();
    let opened = Opened::open(Vec::new());
    opened
        .session
        .ask("asked".into(), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Ok(None));
            let Delivery::Close(close) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
                panic!("ask queues close after its prompt");
            };
            (close.0)(Ok(None));
            Ok(())
        })
        .expect("an accepted first prompt keeps what run returned");
    close_within(opened.session, opened.log);

    reset();
    let opened = Opened::open(Vec::new());
    let failure = opened
        .session
        .ask("asked".into(), Arc::new(|| false), |inbox| {
            (next_prompt(&inbox).0)(Ok(None));
            let Delivery::Close(close) = Deadline::after(DEADLINE).recv(&inbox).unwrap() else {
                panic!("ask queues close after its prompt");
            };
            (close.0)(Ok(None));
            Err(crate::failure(ErrorCode::Busy, "busy"))
        })
        .expect_err("run's own failure wins");
    assert_eq!(failure.code, ErrorCode::Busy);
    close_within(opened.session, opened.log);
}

/// Appends one prompted turn, so `close` keeps the session directory and the
/// test can check `session.lock` afterwards.
fn keep_dir(log: &Arc<Log>) {
    use contract::events::{InputItem, TurnStarted};
    use contract::shapes::{Origin, Sender};
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
}

#[track_caller]
fn assert_lock_released(dir: &std::path::Path) {
    assert!(
        matches!(log::try_hold(dir), Ok(log::Hold::Held(_))),
        "close released session.lock"
    );
}

#[test]
fn close_returns_after_the_socket_path_was_removed() {
    reset();
    let opened = Opened::open(Vec::new());
    keep_dir(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| Ok(()))
        .unwrap();
    fs::remove_file(&opened.socket).unwrap();
    let dir = opened.session.dir.clone();
    close_within(opened.session, opened.log);
    assert_lock_released(&dir);
}

#[test]
fn close_returns_when_the_socket_path_was_rebound() {
    reset();
    let opened = Opened::open(Vec::new());
    keep_dir(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| Ok(()))
        .unwrap();
    fs::remove_file(&opened.socket).unwrap();
    // Another listener answers at the path now: the wake connect reaches it,
    // not this session's accept.
    let _other = UnixListener::bind(&opened.socket).unwrap();
    let dir = opened.session.dir.clone();
    close_within(opened.session, opened.log);
    assert_lock_released(&dir);
}

#[test]
fn a_reader_published_after_stop_is_rejected_not_leaked() {
    reset();
    let opened = Opened::open(Vec::new());
    let gate = Arc::clone(&opened.session.gate);
    // The acceptor passed its stopped check before close began.
    assert!(!gate.conns.stopped());
    let (checked_tx, checked_rx) = mpsc::channel();
    let (stopped_tx, stopped_rx) = mpsc::channel();
    let closer = Arc::clone(&gate);
    thread::spawn(move || {
        Deadline::after(DEADLINE)
            .recv(&checked_rx)
            .expect("the acceptor checked before the stop");
        closer.conns.mark_stopped();
        closer.conns.join_clients();
        stopped_tx.send(()).unwrap_or(());
    });
    checked_tx.send(()).unwrap();
    Deadline::after(DEADLINE)
        .recv(&stopped_rx)
        .expect("the stop and the join land before the publish");
    // The acceptor now publishes after the stop, as the racy loop did: the
    // publication is rejected, its stream shut, its thread ended.
    let (_peer, stream) = UnixStream::pair().unwrap();
    let shutdown = stream.try_clone().unwrap();
    let (_read, stop) = ::support::stoppable::reader(stream).unwrap();
    let shut = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&shut);
    let both = crate::client::ender(shutdown, stop);
    let (id_tx, id_rx) = mpsc::channel::<u64>();
    let (exited_tx, exited_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let _received = id_rx.recv();
        exited_tx.send(()).unwrap_or(());
    });
    let handle = match gate.conns.push_reader(
        reader,
        Box::new(move || {
            flag.store(true, Ordering::SeqCst);
            both();
        }),
    ) {
        Ok(_) => panic!("a reader published after the stop is rejected"),
        Err(handle) => handle,
    };
    assert!(
        shut.load(Ordering::SeqCst),
        "the rejection shuts the stream"
    );
    assert!(
        gate.conns
            .wait_while(DEADLINE, |state| state.live_len() != 0),
        "the rejected reader is never published"
    );
    drop(id_tx);
    Deadline::after(DEADLINE)
        .recv(&exited_rx)
        .expect("the rejected reader's thread ends");
    match handle.join() {
        Ok(()) | Err(_) => {}
    }
    close_within(opened.session, opened.log);
}

#[test]
fn close_connects_nowhere_through_the_socket_path() {
    reset();
    let opened = Opened::open(Vec::new());
    keep_dir(&opened.log);
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| Ok(()))
        .unwrap();
    let socket = opened.socket.clone();
    fs::remove_file(&socket).unwrap();
    let rebound = UnixListener::bind(&socket).unwrap();
    rebound.set_nonblocking(true).unwrap();
    let dir = opened.session.dir.clone();
    close_within(opened.session, opened.log);
    assert_lock_released(&dir);
    match rebound.accept() {
        Ok(_) => panic!("close connected through the rebound socket path"),
        Err(error) => assert_eq!(
            error.kind(),
            std::io::ErrorKind::WouldBlock,
            "the rebound listener stays quiet"
        ),
    }
}

#[test]
fn close_with_nothing_written_and_the_log_held_returns() {
    // A signal while the session process is armed, before `fiber_started`,
    // is recorded and closes with nothing written, while another `Arc<Log>`
    // (the jobs' emit) still holds the log: the printer's watcher never sees
    // the log dropped, so `close` must still return (#830).
    reset();
    let opened = Opened::open(Vec::new());
    let _held = Arc::clone(&opened.log);
    close_within(opened.session, opened.log);
}

#[test]
fn close_prints_durable_lines_dropped_before_stop() {
    // The `STOP` `close` pushes is kept ahead of the catch-up, so ending on
    // it would truncate stdout before the durable lines the queue dropped
    // are recovered, including `fiber_exited` (#830).
    reset();
    // Every write waits for the test's release, so `close`'s `STOP` is
    // queued while the printer still holds undrained lines: without the
    // drain it ends on `STOP` before the catch-up, with it every line
    // below still prints.
    struct GatedOut {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        buf: Arc<Mutex<Vec<u8>>>,
    }
    impl Write for GatedOut {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if let Ok(()) = self.entered.send(()) {}
            // Held until the test releases this write; the waits below
            // name a hang through their deadlines instead of blocking
            // forever.
            if self.release.recv().is_err() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the test went away",
                ));
            }
            lock(&self.buf).extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let temp = fakes::TempDir::new("fd");
    let home = temp.path().join("h");
    let sessions = home.join("projects/p/sessions");
    let id = contract::SessionId(crate::mint("s_"));
    let dir = sessions.join(&id.0);
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id, Arc::clone(&timed)).unwrap());
    let buf = Arc::new(Mutex::new(Vec::new()));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let out = GatedOut {
        entered: entered_tx,
        release: release_rx,
        buf: Arc::clone(&buf),
    };
    let timed: Arc<dyn Clock> = clock;
    let session = Session::open(&home, &dir, &log, timed, Vec::new(), Box::new(out)).unwrap();
    // Observed before the first release below: `close` queues its `STOP`
    // while the printer still holds undrained lines.
    let gate = Arc::clone(&session.gate);
    let (stop_tx, stop_rx) = mpsc::channel();
    *super::lock(&gate.probe) = Some(Arc::new(move |point| {
        if point == Probe::PrinterStopQueued
            && let Ok(()) = stop_tx.send(())
        {}
    }));
    // Prompted, so `close` keeps the session directory: the drain's
    // catch-up re-reads the dropped lines from `events.jsonl`.
    keep_dir(&log);
    // One more durable line than the watcher's queue holds, so some are
    // dropped while the printer is held on its first write.
    const LINES: usize = 1_202;
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("the printer blocks in its first write");
    for _ in 1..LINES - 1 {
        log.append(&step(), None, None).unwrap();
    }
    log.append(&exited(), None, None).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let closing = Arc::clone(&log);
    thread::spawn(move || {
        session.close(closing);
        if let Ok(()) = done_tx.send(()) {}
    });
    drop(log);
    // Forced interleaving: `close` has queued its `STOP` before the printer
    // is released, so the catch-up the drain recovers is really behind it.
    Deadline::after(DEADLINE)
        .recv(&stop_rx)
        .expect("close queues its STOP while the printer is held");
    // One deadline for the whole wait: the release loop and the completion
    // receive below share it. Releases each write in turn until every line
    // is through or the printer went away early; without the drain it ends
    // on `STOP` with most lines still undelivered.
    let wait = Deadline::after(DEADLINE);
    let mut released = 0;
    if release_tx.send(()).is_err() {
        panic!("the printer is still held on its first write");
    }
    released += 1;
    while released < LINES {
        if done_rx.try_recv().is_ok() {
            break;
        }
        match wait.recv(&entered_rx) {
            Ok(()) => {
                if release_tx.send(()).is_err() {
                    break;
                }
                released += 1;
            }
            Err(_) => break,
        }
    }
    wait.recv(&done_rx)
        .expect("close returns after the writer is released");
    let text = String::from_utf8(lock(&buf).clone()).expect("stdout is UTF-8");
    let lines = lines_of(&text);
    let seqs: Vec<u64> = lines
        .iter()
        .filter_map(|line| line["seq"].as_u64())
        .collect();
    let expected: Vec<u64> = (0..LINES as u64).collect();
    assert_eq!(
        seqs, expected,
        "stdout holds every durable line in order, including fiber_exited"
    );
    assert_eq!(
        lines.last().expect("stdout holds fiber_exited")["kind"],
        "fiber_exited"
    );
}
