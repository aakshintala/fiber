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

use contract::clock::Clock;
use contract::events::{CommandAccepted, Empty, Event, ExtensionsLoaded, LoadedExtension, Notice};
use contract::{CommandId, SessionId};
use fakes::Deadline;
use fakes::clock::FakeClock;
use log::Log;

use super::*;

#[allow(
    clippy::duplicate_mod,
    reason = "each unit-test file includes the shared support itself"
)]
#[path = "../../tests/support/mod.rs"]
mod support;

use support::DEADLINE;

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
            replaces: Vec::new(),
        }],
    })
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
    buf_cv: Condvar,
}

impl Held {
    fn waiting() -> Arc<Self> {
        Arc::new(Self {
            mu: Mutex::new(true),
            cv: Condvar::new(),
            entered: Mutex::new(false),
            entered_cv: Condvar::new(),
            buf: Mutex::new(Vec::new()),
            buf_cv: Condvar::new(),
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

    /// The buffer once `ready` holds of it, waited with a deadline: the
    /// writer only appends, so a wait for present lines never passes
    /// early, and an assertion that a later line is still absent holds
    /// because nothing after it was sent yet.
    fn wait_for(&self, ready: impl Fn(&str) -> bool, what: &str) -> String {
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
        self.held.buf_cv.notify_all();
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_lowering_drains_a_lagged_writer_through_the_cutoff() {
    let (_temp, log, id, clock) = open_log();
    let watcher = log.watch_all();
    let outbox = Outbox::new(watcher.injector());
    let held = Held::waiting();
    let (switch_tx, switches) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn({
        let held = Arc::clone(&held);
        move || {
            let ended = crate::client::write_loop(
                watcher,
                Box::new(HeldWrite { held }),
                false,
                switches,
                None,
            );
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
            response: None,
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
            thinking: None,
            state: contract::events::SessionState::Idle,
            since: 0,
            started: 0,
            calls: 0,
            turns: Some(0),
            step: None,
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
            project: "-w".into(),
            clients: 0,
            closing: false,
        }),
        None,
        None,
    )
    .unwrap();
    held.release();
    drop(log);
    outbox.stop(&id);
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
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

#[test]
fn a_line_at_the_cutoff_is_skipped_never_written() {
    let (_temp, log, id, _clock) = open_log();
    log.append(&step(), None, None).unwrap();
    log.append(&step(), None, None).unwrap();
    let cutoff = log.count();
    // Both watchers start at the cutoff, so the next durable line is
    // exactly at the bound. It arrives after the reader read the count,
    // queued in the old watcher ahead of the control line.
    let old = log.watch();
    let new = log.watch();
    log.append(&step(), None, None).unwrap();
    assert_eq!(log.count(), cutoff + 1);
    drop(log);
    let mut writing = Writing {
        watcher: old,
        summary: false,
        cutoff: 0,
        written: None,
    };
    let (tx, rx) = mpsc::channel();
    tx.send(Switch {
        watcher: new,
        summary: true,
        cutoff,
        prelude: vec![control_line(&id, "x.ack", 7)],
        response: None,
    })
    .unwrap();
    let mut buf = Vec::new();
    let drained = apply(&mut writing, &rx, &mut buf, None);
    assert!(
        drained.is_err(),
        "the bound line is skipped, so the drain meets the ended watcher"
    );
    assert!(
        buf.is_empty(),
        "neither the bound line nor the prelude is written"
    );
    assert_eq!(writing.written, None, "nothing below the cutoff was owed");
}

/// A `command_accepted` envelope for `id`, as the deferred settlement carries it.
fn settled_acceptance(id: &SessionId, clock: &FakeClock, command: &str) -> contract::Envelope {
    crate::session::envelope(
        id,
        clock,
        &Event::CommandAccepted(CommandAccepted {
            command_id: CommandId(command.into()),
            result: None,
        }),
    )
}

/// A `notice` envelope, as the deferred settlement carries a switch refusal.
fn settled_notice(id: &SessionId, clock: &FakeClock) -> contract::Envelope {
    crate::session::envelope(
        id,
        clock,
        &Event::Notice(Notice {
            code: contract::ErrorCode::CredentialMissing,
            message: "no credential named `nope`".into(),
            extension: None,
        }),
    )
}

fn envelopes(text: &str) -> Vec<contract::Envelope> {
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_deferred_response_is_written_after_the_drain_before_the_snapshots() {
    let (_temp, log, id, clock) = open_log();
    let old = log.watch_all();
    log.append(&step(), None, None).unwrap();
    log.append(&step(), None, None).unwrap();
    log.append(&extensions(), None, None).unwrap();
    let cutoff = log.count();
    let new = log.watch();
    let snapshot = log.latest("extensions_loaded").unwrap();
    let (tx, rx) = mpsc::channel();
    let (resp_tx, resp_rx) = mpsc::channel();
    tx.send(Switch {
        watcher: new,
        summary: true,
        cutoff,
        prelude: vec![snapshot],
        response: Some(resp_rx),
    })
    .unwrap();
    // The settlement is ready before the switch is applied: the order it
    // is written in still puts the drain first and the snapshots last.
    resp_tx
        .send(vec![
            settled_acceptance(&id, &clock, "c_deferred"),
            settled_notice(&id, &clock),
        ])
        .unwrap();
    drop(resp_tx);
    let mut writing = Writing {
        watcher: old,
        summary: false,
        cutoff: 0,
        written: None,
    };
    let mut buf = Vec::new();
    apply(&mut writing, &rx, &mut buf, None).unwrap();
    let lines = envelopes(&String::from_utf8(buf).unwrap());
    let kinds: Vec<&str> = lines.iter().map(|line| line.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "step_started",
            "step_started",
            "extensions_loaded",
            "command_accepted",
            "notice",
            "extensions_loaded",
        ],
        "the drain, then the deferred response, then the snapshots"
    );
    assert_eq!(
        lines[3]
            .payload
            .get("command_id")
            .and_then(|id| id.as_str()),
        Some("c_deferred")
    );
    assert_eq!(
        lines[4].payload.get("code").and_then(|code| code.as_str()),
        Some("credential_missing")
    );
    assert_eq!(
        lines[5].seq.map(|seq| seq.0),
        Some(cutoff - 1),
        "the snapshot is the latest extensions, not redrained history"
    );
    assert!(writing.summary);
    assert_eq!(writing.cutoff, cutoff);
}

#[test]
fn a_lowering_waits_for_the_deferred_response_after_its_drain() {
    let (_temp, log, id, clock) = open_log();
    let watcher = log.watch_all();
    let outbox = Outbox::new(watcher.injector());
    let held = Held::waiting();
    held.release();
    let (switch_tx, switches) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn({
        let held = Arc::clone(&held);
        move || {
            let ended = crate::client::write_loop(
                watcher,
                Box::new(HeldWrite { held }),
                false,
                switches,
                None,
            );
            if let Ok(()) = done_tx.send(ended) {}
        }
    });
    log.append(&step(), None, None).unwrap();
    log.append(&step(), None, None).unwrap();
    log.append(&extensions(), None, None).unwrap();
    let summary = log.watch();
    let cutoff = log.count();
    let snapshot = log.latest("extensions_loaded").unwrap();
    let injector = summary.injector();
    let (resp_tx, resp_rx) = mpsc::channel();
    switch_tx
        .send(Switch {
            watcher: summary,
            summary: true,
            cutoff,
            prelude: vec![snapshot],
            response: Some(resp_rx),
        })
        .unwrap();
    outbox.swap(control_line(&id, LEVEL, 0), injector);
    // The drain arrives, but no acceptance: the loop has not answered yet,
    // and nothing after the answer can precede it.
    let text = held.wait_for(
        |text| {
            envelopes(text)
                .iter()
                .filter_map(|line| line.seq.map(|seq| seq.0))
                .collect::<Vec<_>>()
                == (0..cutoff).collect::<Vec<_>>()
        },
        "the drain arrives before the deferred response",
    );
    assert!(
        envelopes(&text)
            .iter()
            .all(|line| line.kind != "command_accepted"),
        "no acceptance precedes the loop's answer"
    );
    resp_tx
        .send(vec![settled_acceptance(&id, &clock, "c_lower")])
        .unwrap();
    drop(resp_tx);
    let text = held.wait_for(
        |text| {
            envelopes(text)
                .iter()
                .map(|line| line.kind.as_str())
                .collect::<Vec<_>>()
                == [
                    "step_started",
                    "step_started",
                    "extensions_loaded",
                    "command_accepted",
                    "extensions_loaded",
                ]
        },
        "the drain, then the deferred acceptance, then the snapshot",
    );
    let lines = envelopes(&text);
    let kinds: Vec<&str> = lines.iter().map(|line| line.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "step_started",
            "step_started",
            "extensions_loaded",
            "command_accepted",
            "extensions_loaded",
        ]
    );
    assert_eq!(
        lines[3]
            .payload
            .get("command_id")
            .and_then(|id| id.as_str()),
        Some("c_lower"),
        "the deferred acceptance answers the label-bearing change"
    );
    drop(log);
    outbox.stop(&id);
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
        "the writer ends once the log is dropped"
    );
}

#[test]
fn a_raising_writes_nothing_before_its_deferred_response() {
    let (_temp, log, id, clock) = open_log();
    log.append(&step(), None, None).unwrap();
    let watcher = log.watch();
    let outbox = Outbox::new(watcher.injector());
    let held = Held::waiting();
    held.release();
    let (switch_tx, switches) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn({
        let held = Arc::clone(&held);
        move || {
            let ended = crate::client::write_loop(
                watcher,
                Box::new(HeldWrite { held }),
                true,
                switches,
                None,
            );
            if let Ok(()) = done_tx.send(ended) {}
        }
    });
    // The folded history is seeded before the switch: it must still wait
    // behind the loop's answer.
    let full = log.watch_all_seeded();
    let injector = full.injector();
    let (resp_tx, resp_rx) = mpsc::channel();
    switch_tx
        .send(Switch {
            watcher: full,
            summary: false,
            cutoff: 0,
            prelude: Vec::new(),
            response: Some(resp_rx),
        })
        .unwrap();
    outbox.swap(control_line(&id, LEVEL, 0), injector);
    // No line can precede the answer, not even the seeded history: the
    // writer is parked at the response wait, past every write it could
    // make before the loop's answer (the old-level line was skipped,
    // never written). The buffer is read directly, so no line hides
    // behind a delay and no clock is read.
    crate::session::tests::wait_switch_waiting();
    assert!(
        lock(&held.buf).is_empty(),
        "no line precedes the loop's answer"
    );
    resp_tx
        .send(vec![settled_acceptance(&id, &clock, "c_raise")])
        .unwrap();
    drop(resp_tx);
    let text = held.wait_for(
        |text| {
            envelopes(text)
                .iter()
                .map(|line| line.kind.as_str())
                .collect::<Vec<_>>()
                == ["command_accepted", "step_started"]
        },
        "the deferred acceptance arrives after the loop's answer, before the seeds",
    );
    let lines = envelopes(&text);
    let kinds: Vec<&str> = lines.iter().map(|line| line.kind.as_str()).collect();
    assert_eq!(kinds, ["command_accepted", "step_started"]);
    assert_eq!(
        lines[0]
            .payload
            .get("command_id")
            .and_then(|id| id.as_str()),
        Some("c_raise"),
        "the deferred acceptance answers the label-bearing change"
    );
    drop(log);
    outbox.stop(&id);
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
        "the writer ends once the log is dropped"
    );
}

#[test]
fn a_cancelled_response_ends_the_switch_as_an_error() {
    let (_temp, log, id, _clock) = open_log();
    let old = log.watch_all();
    log.append(&step(), None, None).unwrap();
    let cutoff = log.count();
    let new = log.watch();
    let (tx, rx) = mpsc::channel();
    let (_resp_tx, resp_rx) = mpsc::channel::<Vec<contract::Envelope>>();
    tx.send(Switch {
        watcher: new,
        summary: true,
        cutoff,
        prelude: vec![control_line(&id, "x.ack", 7)],
        response: Some(resp_rx),
    })
    .unwrap();
    // The reader is gone: its cleanup took the sender, closing the wait.
    drop(_resp_tx);
    let mut writing = Writing {
        watcher: old,
        summary: false,
        cutoff: 0,
        written: None,
    };
    let mut buf = Vec::new();
    assert!(
        apply(&mut writing, &rx, &mut buf, None).is_err(),
        "a cancelled wait ends the writer instead of hanging its join"
    );
    let lines = envelopes(&String::from_utf8(buf).unwrap());
    assert_eq!(lines.len(), 1, "the drain is written; nothing after it");
    assert_eq!(lines[0].kind, "step_started");
}

/// A writer whose bytes the test reads back.
struct SharedOut(Arc<Mutex<Vec<u8>>>);

impl Write for SharedOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        lock(&self.0).extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": every line a session reads gets exactly one answer on its connection before the session shuts it.
#[test]
fn a_kept_answer_pushed_after_the_log_ends_is_written_before_stop() {
    use contract::events::{CommandRejected, Event};

    let opened = support::Opened::open(Vec::new());
    let gate = Arc::clone(opened.session.gate_for_test());
    let watcher = opened.log.watch_all();
    let injector = watcher.injector();
    let conn = gate
        .conns
        .push_reader(thread::spawn(|| {}), Box::new(|| {}), Box::new(|| {}))
        .expect("the gate is running");
    let reader = thread::current().id();
    gate.accepted.track(reader);
    gate.conns.note_owed(conn, reader);
    // The flight's probe is the only signal the writer reached past the
    // end: the writer notes it before every `recv_after_end`, so a line
    // pushed after this wait arrives past the end, never before it
    // (`docs/testing.md`, "Waits and timeouts").
    let (past_tx, past_end) = mpsc::channel();
    *lock(&gate.probe) = Some(Arc::new(move |point| {
        if point == crate::session::tests::Probe::WriterPastEnd
            && let Ok(()) = past_tx.send(())
        {}
    }));
    let flight = crate::client::Flight {
        gate: Arc::clone(&gate),
        conn,
        reader,
    };
    let (_switch_tx, switches) = mpsc::channel();
    let out = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn({
        let out = Arc::clone(&out);
        move || {
            let ended = crate::client::write_loop(
                watcher,
                Box::new(SharedOut(out)),
                false,
                switches,
                Some(&flight),
            );
            if let Ok(()) = done_tx.send(ended) {}
        }
    });
    opened.log.end_watchers();
    Deadline::after(DEADLINE)
        .recv(&past_end)
        .expect("the writer reads past the log's end");
    let answer = crate::session::envelope(
        &gate.session_id,
        opened.clock.as_ref(),
        &Event::CommandRejected(CommandRejected {
            command_id: Some(CommandId("c_held".into())),
            code: contract::ErrorCode::Closing,
            message: crate::client::ENDED.into(),
        }),
    );
    injector.push_kept(answer);
    injector.push_kept(control_line(&gate.session_id, crate::client::STOP, 0));
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
        "the writer returns once STOP follows the answer"
    );
    drop(_switch_tx);
    let lines = envelopes(&String::from_utf8(lock(&out).clone()).unwrap());
    assert_eq!(
        lines.len(),
        1,
        "the kept answer pushed after the end is written before stop; got {lines:?}"
    );
    assert_eq!(lines[0].kind, "command_rejected");
    gate.conns.finish(conn);
    support::close_within(opened.session, opened.log);
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
#[test]
fn a_level_change_after_the_log_ends_moves_the_writer_to_the_new_watcher() {
    let (_temp, log, id, clock) = open_log();
    let watcher = log.watch_all();
    let old = watcher.injector();
    let (switch_tx, switches) = mpsc::channel();
    let out = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn({
        let out = Arc::clone(&out);
        move || {
            let ended =
                crate::client::write_loop(watcher, Box::new(SharedOut(out)), false, switches, None);
            if let Ok(()) = done_tx.send(ended) {}
        }
    });
    log.end_watchers();
    // A level change that raced the close: its watcher is registered after
    // the end, so it starts ended; its prelude and its queue are written.
    let fresh = log.watch();
    let fresh_injector = fresh.injector();
    switch_tx
        .send(Switch {
            watcher: fresh,
            summary: false,
            cutoff: 0,
            prelude: vec![settled_acceptance(&id, &clock, "c_up")],
            response: None,
        })
        .unwrap();
    old.push_kept(control_line(&id, LEVEL, 0));
    fresh_injector.push_kept(settled_acceptance(&id, &clock, "c_after"));
    fresh_injector.push_kept(control_line(&id, crate::client::STOP, 0));
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx).is_ok(),
        "the writer follows the switch and ends at STOP on the new queue"
    );
    let lines = envelopes(&String::from_utf8(lock(&out).clone()).unwrap());
    let ids: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.payload.get("command_id").and_then(|id| id.as_str()))
        .collect();
    assert_eq!(ids, ["c_up", "c_after"], "{lines:?}");
}
