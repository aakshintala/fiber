//! Unit tests for the level switch: the outbox order around the control
//! line, the no-op switch, and the downgrade drain around `cutoff`, with a
//! held writer as the barrier.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::Write;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::events::{CommandAccepted, Empty, Event, ExtensionsLoaded, LoadedExtension, Notice};
use contract::{CommandId, SessionId};
use fakes::clock::FakeClock;
use log::Log;

use super::*;

/// A hang bound for one wait: every wait names what it waits for.
const DEADLINE: Duration = Duration::from_secs(10);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn open_log() -> (fakes::TempDir, Arc<Log>, SessionId, Arc<FakeClock>) {
    let temp = fakes::TempDir::new("fd");
    let sessions = temp.path().join("h/projects/p/sessions");
    let id = SessionId(crate::mint("s_"));
    let clock = FakeClock::new();
    let timed = Arc::clone(&clock);
    let timed: Arc<dyn Clock> = timed;
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
    (temp, log, id, clock)
}

fn step() -> Event {
    Event::StepStarted(Empty {})
}

fn notice() -> Event {
    Event::Notice(Notice {
        code: contract::ErrorCode::IoFailed,
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

fn token(line: &contract::Envelope) -> u64 {
    line.payload
        .get("token")
        .and_then(|token| token.as_u64())
        .unwrap()
}

#[test]
fn outbox_swap_orders_lines_around_the_control_line() {
    let (_temp, log, id, _clock) = open_log();
    let old = log.watch();
    let new = log.watch();
    let outbox = Outbox::new(old.injector());
    outbox.push_kept(control_line(&id, "x.one", 1));
    outbox.swap(control_line(&id, LEVEL, 0), new.injector());
    outbox.push_kept(control_line(&id, "x.two", 2));
    let mut old = old;
    let mut new = new;
    // A line pushed before the swap lands in the old queue ahead of the
    // control line; one pushed after lands only in the new queue.
    let first = old.try_recv().unwrap().unwrap();
    assert_eq!(first.kind, "x.one");
    assert_eq!(token(&first), 1);
    let control = old.try_recv().unwrap().unwrap();
    assert_eq!(control.kind, LEVEL);
    assert!(old.try_recv().unwrap().is_none());
    let second = new.try_recv().unwrap().unwrap();
    assert_eq!(second.kind, "x.two");
    assert_eq!(token(&second), 2);
    assert!(new.try_recv().unwrap().is_none());
}

#[test]
fn apply_with_no_switch_pending_changes_nothing() {
    let (_temp, log, _id, _clock) = open_log();
    let watcher = log.watch();
    let mut writing = Writing {
        watcher,
        summary: true,
        cutoff: 7,
        written: Some(3),
    };
    // An empty channel: the fast-fail path that keeps a mutant from
    // hanging.
    let (_tx, rx) = mpsc::channel();
    let mut buf = Vec::new();
    apply(&mut writing, &rx, &mut buf).unwrap();
    assert!(buf.is_empty(), "nothing is written without a switch");
    assert!(writing.summary);
    assert_eq!(writing.cutoff, 7);
    assert_eq!(writing.written, Some(3));
    // A disconnected channel changes nothing either.
    let (tx, rx) = mpsc::channel();
    drop(tx);
    apply(&mut writing, &rx, &mut buf).unwrap();
    assert!(buf.is_empty(), "nothing is written without a switch");
    assert!(writing.summary);
    assert_eq!(writing.cutoff, 7);
    assert_eq!(writing.written, Some(3));
}

#[test]
fn the_drain_condition_holds_at_its_edges() {
    assert!(!drain_more(None, 0), "no cutoff drains nothing");
    assert!(drain_more(None, 1), "one line is owed");
    assert!(!drain_more(Some(4), 5), "cutoff - 1 is done");
    assert!(drain_more(Some(3), 5), "cutoff - 2 still drains");
}

/// A writer that blocks in its first write until released, so the test
/// puts the lag and the switch in each order that matters. Copies the
/// `Held`/`HeldWrite` pattern in `session_tests.rs`.
struct Held {
    mu: Mutex<bool>,
    cv: Condvar,
    entered: Mutex<bool>,
    entered_cv: Condvar,
    buf: Mutex<Vec<u8>>,
}

impl Held {
    fn waiting() -> Arc<Self> {
        Arc::new(Self {
            mu: Mutex::new(true),
            cv: Condvar::new(),
            entered: Mutex::new(false),
            entered_cv: Condvar::new(),
            buf: Mutex::new(Vec::new()),
        })
    }

    fn wait_blocked(&self) {
        let guard = lock(&self.entered);
        let (guard, _) = self
            .entered_cv
            .wait_timeout_while(guard, DEADLINE, |entered| !*entered)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(*guard, "the writer is blocked in write");
    }

    fn release(&self) {
        *lock(&self.mu) = false;
        self.cv.notify_all();
    }
}

struct HeldWrite {
    held: Arc<Held>,
}

impl Write for HeldWrite {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        *lock(&self.held.entered) = true;
        self.held.entered_cv.notify_all();
        let guard = lock(&self.held.mu);
        let (guard, waited) = self
            .held
            .cv
            .wait_timeout_while(guard, DEADLINE, |held| *held)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            !waited.timed_out() && !*guard,
            "the writer was released before its deadline"
        );
        drop(guard);
        lock(&self.held.buf).extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_lowering_drains_a_lagged_writer_through_the_cutoff() {
    let (_temp, log, id, clock) = open_log();
    let watcher = log.watch_all().unwrap();
    let outbox = Outbox::new(watcher.injector());
    let held = Held::waiting();
    let (switch_tx, switches) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn({
        let held = Arc::clone(&held);
        move || {
            let ended =
                crate::client::write_loop(watcher, Box::new(HeldWrite { held }), false, switches);
            if let Ok(()) = done_tx.send(ended) {}
        }
    });
    log.append(&step(), None, None).unwrap();
    held.wait_blocked();
    // The queue lags: everything past its capacity is dropped, and the
    // catch-up the lag flag forces re-reads it.
    for _ in 0..1_100 {
        log.append(&step(), None, None).unwrap();
    }
    // Registered before these lines, so the new queue holds durable lines
    // below `cutoff`: after the switch they must be skipped, having
    // already reached the client at full level.
    let summary = log.watch();
    log.append(&extensions(), None, None).unwrap();
    log.append(&step(), None, None).unwrap();
    let cutoff = log.count();
    let ack = crate::session::envelope(
        &id,
        clock.as_ref(),
        &Event::CommandAccepted(CommandAccepted {
            command_id: CommandId("c_a_down".into()),
            result: None,
        }),
    );
    let latest = log.latest("extensions_loaded").unwrap();
    let injector = summary.injector();
    switch_tx
        .send(Switch {
            watcher: summary,
            summary: true,
            cutoff,
            prelude: vec![ack, latest],
        })
        .unwrap();
    outbox.swap(control_line(&id, LEVEL, 0), injector);
    log.append(&notice(), None, None).unwrap();
    log.append(
        &Event::SessionStatus(contract::events::SessionStatus {
            name: "two".into(),
            workspace: "/w".into(),
            parent: None,
            model: "m".into(),
            state: contract::events::SessionState::Idle,
            since: 0,
            git: None,
            context: None,
            spend: contract::shapes::Usage {
                tokens: contract::shapes::Tokens {
                    input: 0,
                    cache_read: 0,
                    cache_write: std::collections::BTreeMap::new(),
                    output: 0,
                },
                cost: Some(0.0),
                subscription_cost: 0.0,
            },
            delegates: 0,
            jobs: 0,
        }),
        None,
        None,
    )
    .unwrap();
    held.release();
    drop(log);
    assert!(
        done_rx.recv_timeout(DEADLINE).is_ok(),
        "the writer ends once the log is dropped"
    );
    let text = String::from_utf8(lock(&held.buf).clone()).unwrap();
    let lines: Vec<contract::Envelope> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let ack_at = lines
        .iter()
        .position(|line| {
            line.kind == "command_accepted"
                && line.payload.get("command_id").and_then(|id| id.as_str()) == Some("c_a_down")
        })
        .expect("the acknowledgement is written");
    // Every durable line below the cutoff arrives exactly once, before
    // the acknowledgement, with no gaps.
    let before: Vec<u64> = lines[..ack_at]
        .iter()
        .filter_map(|line| line.seq.map(|seq| seq.0))
        .collect();
    assert_eq!(before, (0..cutoff).collect::<Vec<_>>());
    // After it, only the prelude's snapshot and the status: the queued
    // `extensions_loaded` below the cutoff is not written again from the
    // new queue, and the notice is not written.
    let after: Vec<&str> = lines[ack_at + 1..]
        .iter()
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(after, ["extensions_loaded", "session_status"]);
    let durable_extensions: Vec<&contract::Envelope> = lines
        .iter()
        .filter(|line| line.kind == "extensions_loaded" && line.seq.is_some())
        .collect();
    assert_eq!(
        durable_extensions.len(),
        2,
        "the extensions below the cutoff arrive once at full level, and once \
         as the prelude snapshot, which is exempt: never again from the new queue"
    );
    let full_copy = lines[..ack_at]
        .iter()
        .position(|line| line.kind == "extensions_loaded")
        .expect("the full-level copy arrives before the acknowledgement");
    assert_eq!(lines[full_copy].seq.map(|seq| seq.0), Some(cutoff - 2));
    assert!(
        std::ptr::eq(&lines[ack_at + 1], durable_extensions[1]),
        "the prelude snapshot is the first line after the acknowledgement"
    );
}
