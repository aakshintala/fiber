//! The keeper workload's unit tests: the script's shape and the injected
//! core (`poll_keeper`, `keeper_rss_kib`) on a parking fake clock. Every
//! `clock.sleep` is stepped through `FakeClock::advance_when_parked` naming
//! the worker thread and its expected `origin + n * PROBE` instant: each
//! injected closure signals its read first, so a step proves the worker
//! reached the wait past its own expiry check before time moves
//! (`docs/testing.md`, "Waits and timeouts").
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failure is the test's"
)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::json;
use support::clock::Sleep;

use super::{keeper_rss_kib, keeper_script, poll_keeper};
use crate::linux::Counts;
use crate::run::PROBE;

/// The wall-clock bound on the test's own waits: a worker that never reads
/// or finishes fails the test instead of hanging it.
const WALL: Duration = Duration::from_secs(30);

/// The worker's events: each injected read as it happens, then its result.
enum Event<T> {
    Read(&'static str),
    Done(Result<T, String>),
}

/// A parking clock, its worker thread and the worker's events.
struct Driven<T> {
    clock: Arc<FakeClock>,
    worker: thread::JoinHandle<()>,
    events: mpsc::Receiver<Event<T>>,
}

impl<T: Send + 'static> Driven<T> {
    /// Runs `run` on a worker thread with a parking clock, reporting every
    /// injected read and its result.
    fn spawn(
        run: impl FnOnce(mpsc::Sender<Event<T>>, Arc<FakeClock>) -> Result<T, String> + Send + 'static,
    ) -> Self {
        let clock = FakeClock::with_sleep(Sleep::Waits);
        let (tx, events) = mpsc::channel();
        let worker_clock = Arc::clone(&clock);
        let worker = thread::spawn(move || {
            let result = run(tx.clone(), worker_clock);
            tx.send(Event::Done(result)).unwrap_or(());
        });
        Self {
            clock,
            worker,
            events,
        }
    }

    /// Takes the next read signal, failing the test on anything else: reads
    /// happen in the order the waits run them.
    fn read(&self, which: &'static str) {
        match Deadline::after(WALL).recv(&self.events) {
            Ok(Event::Read(got)) => assert_eq!(got, which, "reads happen in order"),
            Ok(Event::Done(_)) => panic!("the worker finished before reading {which}"),
            Err(_) => panic!("the worker never read {which}"),
        }
    }

    /// Steps the worker past its `PROBE` sleep at `at`, naming its thread.
    fn step(&self, at: Instant) {
        let deadline = Deadline::after(WALL);
        self.clock
            .advance_when_parked(Some(at), Some(self.worker.thread().id()), PROBE, &deadline);
    }

    /// Takes the worker's result, failing the test on another read: a read
    /// past the last step means the wait retried instead of ending.
    fn done(self) -> Result<T, String> {
        match Deadline::after(WALL).recv(&self.events) {
            Ok(Event::Done(result)) => {
                self.worker.join().unwrap_or(());
                result
            }
            Ok(Event::Read(which)) => panic!("the worker read {which} past the last step"),
            Err(_) => panic!("the worker never finished"),
        }
    }
}

/// One sleeping thread's counters.
fn sleeping(voluntary: u64) -> BTreeMap<u32, Counts> {
    BTreeMap::from([(
        4121,
        Counts {
            voluntary,
            involuntary: 0,
            name: Some("fiber".to_owned()),
            state: 'S',
        },
    )])
}

/// A status with a 10,840 KiB peak, the shape `linux.rs` parses.
const STATUS: &str = "Name:\tfiber\nState:\tS (sleeping)\nVmHWM:\t   10840 kB\nVmRSS:\t   10812 kB\nRssAnon:\t    4100 kB\nRssFile:\t    4800 kB\n";

#[test]
fn the_script_runs_one_background_shell_call_then_text() {
    let script = keeper_script();
    assert_eq!(script.len(), 2, "one shell call, then the closing text");
    let call = String::from_utf8_lossy(&script[0].body);
    assert!(call.contains("\"shell\""), "the call runs shell: {call}");
    assert!(
        call.contains("sleep 180"),
        "the job outlasts the pre-read waits: {call}"
    );
    assert!(
        call.contains("run_in_background"),
        "the job runs in the background: {call}"
    );
    let text = String::from_utf8_lossy(&script[1].body);
    assert!(
        text.contains("the job is running"),
        "the turn ends with text: {text}"
    );
}

#[test]
fn the_poll_returns_the_one_keeper_pid() {
    let driven = Driven::spawn(|tx, clock| {
        let calls = Cell::new(0);
        poll_keeper(&*clock, 3 * PROBE, || {
            tx.send(Event::Read("find")).unwrap_or(());
            calls.set(calls.get() + 1);
            Ok(if calls.get() == 1 { vec![] } else { vec![42] })
        })
    });
    driven.read("find");
    driven.step(driven.clock.origin() + PROBE);
    driven.read("find");
    assert_eq!(driven.done(), Ok(42));
}

#[test]
fn an_empty_pid_poll_errs_at_its_bound() {
    let driven = Driven::spawn(|tx, clock| {
        poll_keeper(&*clock, 3 * PROBE, || {
            tx.send(Event::Read("find")).unwrap_or(());
            Ok(vec![])
        })
    });
    let origin = driven.clock.origin();
    for n in 1..=3u32 {
        driven.read("find");
        driven.step(origin + n * PROBE);
    }
    driven.read("find");
    let err = driven.done().unwrap_err();
    assert!(err.starts_with("timed out waiting for"), "{err}");
    assert!(err.contains("the keeper to start"), "{err}");
}

#[test]
fn two_pids_err_at_once_with_no_retry() {
    let driven = Driven::spawn(|tx, clock| {
        poll_keeper(&*clock, 3 * PROBE, || {
            tx.send(Event::Read("find")).unwrap_or(());
            Ok(vec![7, 9])
        })
    });
    driven.read("find");
    // A retry would read again past the last step; the error ends the poll.
    assert_eq!(
        driven.done().unwrap_err(),
        "more than one keeper matches: [7, 9]",
    );
}

#[test]
fn settled_threads_read_the_canned_peak() {
    let driven = Driven::spawn(|tx, clock| {
        keeper_rss_kib(
            &*clock,
            3 * PROBE,
            || {
                tx.send(Event::Read("threads")).unwrap_or(());
                Ok(sleeping(1))
            },
            || {
                tx.send(Event::Read("status")).unwrap_or(());
                Ok(STATUS.to_owned())
            },
        )
    });
    let origin = driven.clock.origin();
    driven.read("threads");
    driven.step(origin + PROBE);
    driven.read("threads");
    driven.read("status");
    assert_eq!(
        driven.done().expect("settled threads read the peak"),
        vec![("keeper_rss_kib", json!(10840))],
    );
}

#[test]
fn a_status_without_a_peak_errs() {
    let driven = Driven::spawn(|tx, clock| {
        keeper_rss_kib(
            &*clock,
            3 * PROBE,
            || {
                tx.send(Event::Read("threads")).unwrap_or(());
                Ok(sleeping(1))
            },
            || {
                tx.send(Event::Read("status")).unwrap_or(());
                Ok("Name:\tfiber\nState:\tS (sleeping)\n".to_owned())
            },
        )
    });
    let origin = driven.clock.origin();
    driven.read("threads");
    driven.step(origin + PROBE);
    driven.read("threads");
    driven.read("status");
    let err = driven.done().unwrap_err();
    assert!(err.contains("VmHWM"), "{err}");
}
