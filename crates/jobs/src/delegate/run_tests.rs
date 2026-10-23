//! Tests first for the runner: spawning, the drain, the watch loop, the
//! stop timer and the wait, with fake children and a fake watch.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::VecDeque;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Barrier, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::delegate::group::listed;
use contract::Envelope;
use contract::clock::Clock as _;
use contract::events::{FiberExited, FinalMessage, Outcome};
use contract::inbox::Delivery;
use contract::jobs::Jobs as _;
use contract::shapes::{Failure, Usage};
use contract::{ActionId, ErrorCode, JobId, PreSessionExit, Seq, SessionId};
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::{
    MUST_SUCCEED_WITHIN, Recorder, TempDir, Watchdog, group_empties, kill_pid, pids_exit, within,
};

use super::{
    Fold, Fresh, Launch, Launched, POLL, Runner, Shared, Start, Stat, Watch, Watched, Watcher,
    cap_due, kill_due, mint_session_id, note_seq, over_len, park_due,
};
use crate::delegate::outcome::Termination;
use crate::registry::Registry;
use crate::support::{exited, usage};
use crate::timing_tests::{Step, try_step};
use support::clock::Parker;

/// How long a test waits on the wall clock before it fails.
const DEADLINE: Duration = Duration::from_secs(3);

/// The stop bound the tests run with, on the fake clock.
const BOUND: Duration = Duration::from_secs(5);

/// A fake watch: replays queued replies and records when it ran.
struct Script {
    clock: Arc<FakeClock>,
    calls: Mutex<Vec<std::time::Instant>>,
    /// Notified on every recorded call.
    called: Condvar,
    /// The boundary each call passed.
    afters: Mutex<Vec<Option<u64>>>,
    replies: Mutex<VecDeque<WatchReply>>,
    /// Sent once after each `Closed` or `Exited` call delivers its lines:
    /// `on_line` already returned, so the runner folded the envelope.
    folded_tx: mpsc::Sender<()>,
    folded_rx: Mutex<mpsc::Receiver<()>>,
}

enum WatchReply {
    Closed(Vec<Envelope>),
    Exited(Vec<Envelope>),
    /// Like `Exited`, but blocks until the test releases the gate before
    /// returning: the test holds the runner between its result take and
    /// its launch decision, releases the gate, waits for the fold, then
    /// resumes the runner, so the fold always lands inside the pause.
    /// Dropping the sender releases it too, so a failed test never leaves
    /// the watcher parked.
    Gated(Vec<Envelope>, mpsc::Receiver<()>),
    /// Blocks until the test releases the gate, at most the wall bound:
    /// the runner must supervise without it. Dropping the sender releases
    /// it too, so a failed test never leaves the watcher parked.
    Block(mpsc::Receiver<()>),
}

impl Script {
    fn scripted(clock: &Arc<FakeClock>, replies: Vec<WatchReply>) -> Arc<Self> {
        let (folded_tx, folded_rx) = mpsc::channel();
        Arc::new(Self {
            clock: Arc::clone(clock),
            calls: Mutex::new(Vec::new()),
            called: Condvar::new(),
            afters: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
            folded_tx,
            folded_rx: Mutex::new(folded_rx),
        })
    }

    fn watch(
        self: &Arc<Self>,
        _id: &SessionId,
        after: Option<u64>,
        on_line: &mut dyn FnMut(&Envelope),
    ) -> io::Result<Watched> {
        self.calls.lock().unwrap().push(self.clock.now());
        self.afters.lock().unwrap().push(after);
        self.called.notify_all();
        // As `doors::watch_after`: lines at or before the boundary belong
        // to an earlier run, so they are neither delivered nor end the
        // watch.
        let deliver = |line: &Envelope| {
            line.seq
                .map(|seq| after.is_none_or(|at| seq.0 > at))
                .unwrap_or(true)
        };
        // As the real watch: the first delivered `fiber_exited`
        // ends the watch, so a replayed earlier run's end never
        // stands in for this run's.
        let mut deliver_lines = |lines: &[Envelope]| {
            for line in lines.iter().filter(|line| deliver(line)) {
                on_line(line);
                if line.kind == "fiber_exited" {
                    break;
                }
            }
        };
        match self.replies.lock().unwrap().pop_front() {
            None => Err(io::Error::other("refused")),
            Some(WatchReply::Closed(lines)) => {
                for line in lines.iter().filter(|line| deliver(line)) {
                    on_line(line);
                }
                let _sent = self.folded_tx.send(());
                Ok(Watched::Closed)
            }
            Some(WatchReply::Exited(lines)) => {
                deliver_lines(&lines);
                let _sent = self.folded_tx.send(());
                Ok(Watched::Exited)
            }
            Some(WatchReply::Gated(lines, gate)) => {
                // The gate holds the return until the test releases it;
                // the fold lands after.
                deliver_lines(&lines);
                let _sent = self.folded_tx.send(());
                let _released = gate.recv();
                Ok(Watched::Exited)
            }
            Some(WatchReply::Block(gate)) => {
                let _released = gate.recv();
                Err(io::Error::other("released"))
            }
        }
    }

    fn calls(&self) -> Vec<std::time::Instant> {
        self.calls.lock().unwrap().clone()
    }

    /// The boundary each call passed, in call order.
    fn afters(&self) -> Vec<Option<u64>> {
        self.afters.lock().unwrap().clone()
    }

    /// Waits, at most `within` of real time, until the watch ran `n`
    /// times. True once it has; false at the deadline.
    fn await_calls(&self, n: usize, within: Duration) -> bool {
        let (calls, _) = self
            .called
            .wait_timeout_while(self.calls.lock().unwrap(), within, |calls| calls.len() < n)
            .unwrap();
        calls.len() >= n
    }
}

fn envelope(seq: u64, kind: &str, payload: serde_json::Value) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s_child".into()),
        ts: 0,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(seq)),
        payload: payload.as_object().unwrap().clone(),
    }
}

fn fiber_exited_line(seq: u64, text: &str) -> Envelope {
    envelope(
        seq,
        "fiber_exited",
        serde_json::to_value(exited(text)).unwrap(),
    )
}

fn json_line(seq: u64, text: &str) -> String {
    serde_json::to_string(&fiber_exited_line(seq, text)).unwrap()
}

struct Rig {
    _dir: TempDir,
    registry: Arc<Registry>,
    clock: Arc<FakeClock>,
    inbox: mpsc::Receiver<Delivery>,
    script: Arc<Script>,
    job: JobId,
    events: std::path::PathBuf,
}

fn rig(replies: Vec<WatchReply>) -> Rig {
    let dir = TempDir::new("fiber-delegate-run");
    let clock = FakeClock::new();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let registry = Registry::new(
        artifacts,
        Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        Arc::new(Recorder::default()),
    );
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(sessions.join("s_child")).unwrap();
    let script = Script::scripted(&clock, replies);
    Rig {
        _dir: dir,
        registry,
        clock,
        inbox: rx,
        script,
        job: crate::registry::mint_job_id(),
        events: sessions.join("s_child").join("events.jsonl"),
    }
}

impl Rig {
    /// Mirrors the tool's order: build the runner, spawn, record, drive.
    /// Returns the runner's stop, so tests stop through the registry as
    /// `jobs stop` does.
    fn start(&self, shell: &str, bound: Duration, cap: u64) -> mpsc::Receiver<()> {
        self.start_after(shell, bound, cap, None)
    }

    /// [`Rig::start`], watching past `after`.
    fn start_after(
        &self,
        shell: &str,
        bound: Duration,
        cap: u64,
        after: Option<u64>,
    ) -> mpsc::Receiver<()> {
        self.start_timeout(shell, bound, cap, after, None, None).0
    }

    /// Builds the runner, spawns, records and drives it. `on_usage` is
    /// the watcher's tap; `None` keeps the existing unwired behavior.
    fn start_timeout(
        &self,
        shell: &str,
        bound: Duration,
        cap: u64,
        after: Option<u64>,
        timeout: Option<Duration>,
        on_usage: Option<super::UsageForward>,
    ) -> (mpsc::Receiver<()>, Arc<Parker>) {
        let shell = shell.to_owned();
        let script = Arc::clone(&self.script);
        let watch = Arc::new(
            move |id: &SessionId, after: Option<u64>, on_line: &mut dyn FnMut(&Envelope)| {
                script.watch(id, after, on_line)
            },
        );
        let events = self.events.clone();
        let launch: Launch = Arc::new(move |_: &Launched| {
            let mut command = Command::new("sh");
            command.args(["-c", shell.as_str()]);
            command
        });
        let (runner, stop) = Runner::new(
            self.job.clone(),
            SessionId("s_child".into()),
            events.clone(),
            Arc::clone(&self.clock) as Arc<dyn contract::clock::Clock>,
            bound,
            cap,
            watch,
            after,
            timeout,
            on_usage,
        );
        let launched = Launched {
            depth: 1,
            session_id: SessionId("s_child".into()),
            job_id: self.job.clone(),
            parent: SessionId("s_parent".into()),
            prompt: "hi".into(),
            workspace: self.events.parent().unwrap().to_path_buf(),
            start: Start::Fresh(Fresh::Spawn {
                model: "fake/m".into(),
                credential: None,
            }),
        };
        let (child, listing) = runner.spawn(&launch, &launched).unwrap();
        let (_started, finish) = self.registry.open_started(
            self.job.clone(),
            "delegate_spawn".into(),
            "scan".into(),
            events.to_string_lossy().into_owned(),
            stop,
            crate::support::delegate_started(&self.job.0),
        );
        let park = Arc::clone(&runner.park);
        let (done, waited) = mpsc::channel();
        thread::spawn(move || {
            runner.drive(child, Some(listing), finish);
            let _sent = done.send(());
        });
        (waited, park)
    }
}

/// The runner's park when it lies ahead of `now()`, otherwise `None`.
/// Never blocks and never moves the clock: the caller advances only from
/// the returned `until` (docs/testing.md, "Waits and timeouts": a test
/// advances a fake clock only after a signal that the code under test is
/// waiting on that clock). Panics naming `parked()` when more than one
/// thread is parked or a park has no deadline: the runner is the only
/// thread that parks on the rig's clock in these tests.
fn park_ahead(clock: &FakeClock) -> Option<std::time::Instant> {
    let parked = clock.parked();
    if parked.len() > 1 {
        panic!("expected the runner's single park, saw parked={parked:?}");
    }
    match parked.into_iter().next() {
        None => None,
        Some(None) => panic!("the runner parked with no deadline; parked=[None]"),
        Some(Some(until)) => {
            let now = clock.now();
            (until > now).then_some(until)
        }
    }
}

/// The first notice the runner sends. A scoped driver thread advances the
/// fake clock only from the runner's own park (docs/testing.md, "Waits
/// and timeouts"): a park at most one poll ahead moves time to its
/// `until`; a park further ahead is the drain wait and is never advanced,
/// because the drain's end wakes the runner itself; with no park ahead
/// the driver yields. The test thread's single `Deadline` receive
/// is the wall-clock deadline for the whole wait, driver included
/// (docs/testing.md, "Waits and timeouts": every wait has a deadline on
/// the wall clock). The stop flag is set on every outcome of that
/// receive, before the result is inspected, so the driver always ends
/// within `DEADLINE` and the scope never outlives the wait.
#[track_caller]
fn reported(rig: &Rig) -> contract::inbox::JobNotice {
    reported_before(rig, None)
}

/// Takes the runner's report with one wall-clock deadline for the whole
/// wait, failing at the caller's line: `#[track_caller]` reports the
/// test's line, which the `thread::scope` closure below cannot. `expiry`
/// names the wait that never reported. `DelegateUsage` deliveries the
/// watcher's tap forwards ahead of the report are stashed in `usages`
/// and the take loops past them until the `Job` notice arrives.
#[track_caller]
fn take_report(
    rig: &Rig,
    stop: &AtomicBool,
    expiry: &str,
    usages: &mut Vec<(SessionId, String)>,
) -> contract::inbox::JobNotice {
    let deadline = Deadline::after(DEADLINE);
    loop {
        match deadline.recv(&rig.inbox) {
            Ok(Delivery::Job(notice)) => {
                stop.store(true, Ordering::Relaxed);
                return notice;
            }
            Ok(Delivery::DelegateUsage { session, line }) => {
                usages.push((session, line.generation_id.0.clone()));
            }
            Ok(other) => {
                stop.store(true, Ordering::Relaxed);
                panic!("expected the runner's report, got {other:?}");
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                panic!(
                    "{expiry}; now={:?} parked={:?}",
                    rig.clock.now(),
                    rig.clock.parked()
                );
            }
        }
    }
}

/// Advances the fake clock only from the runner's own park, until `stop`
/// is set or `deadline` runs out (docs/testing.md, "Waits and timeouts":
/// a test advances a fake clock only after a signal that the code under
/// test is waiting on that clock). A park at or past `horizon` wakes the
/// runner with a zero advance, which moves no time but wakes every
/// subscriber; `None` advances every park at most one poll ahead. The
/// test thread's single `Deadline` receive is the wall-clock deadline for
/// the whole wait, driver included. The stop flag is set on every outcome
/// of that receive, before the result is inspected, so the driver always
/// ends within `deadline` and the scope never outlives the wait.
fn drive_runner_clock(
    stop: &AtomicBool,
    clock: &Arc<FakeClock>,
    deadline: &Deadline,
    horizon: Option<std::time::Instant>,
) {
    let mut carried: Option<Instant> = None;
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if deadline.left().is_zero() {
            panic!(
                "the runner did not report; now={:?} parked={:?}",
                clock.now(),
                clock.parked()
            );
        }
        let mut candidate = carried.take();
        if candidate.is_some_and(|watched| watched <= clock.now()) {
            candidate = None;
        }
        match candidate.or_else(|| park_ahead(clock)) {
            Some(until) => {
                if horizon.is_some_and(|horizon| until >= horizon) {
                    // Fake time never reaches `horizon`: a zero
                    // advance wakes the parked pass without moving
                    // time, staying short of the park.
                    match try_step(clock, until, Duration::ZERO) {
                        Step::Moved | Step::NoPark => {}
                        Step::Next(watched) => {
                            carried = Some(watched);
                        }
                    }
                } else {
                    let now = clock.now();
                    match until.checked_duration_since(now) {
                        Some(gap) if gap <= super::POLL => match try_step(clock, until, gap) {
                            Step::Moved | Step::NoPark => {}
                            Step::Next(watched) => {
                                carried = Some(watched);
                            }
                        },
                        _ => thread::yield_now(),
                    }
                }
            }
            None => thread::yield_now(),
        }
    }
}

/// `reported` with a horizon: fake time never reaches `horizon`. A park
/// at or past it wakes the runner with a zero advance, which moves no
/// time but wakes every subscriber, so a leader whose stdout EOF beat its
/// zombie state is still reaped on a later pass (docs/testing.md,
/// "Waits and timeouts": deadlines are hang guards, never timing
/// assertions; no timing is asserted here).
#[track_caller]
fn reported_before(rig: &Rig, horizon: Option<std::time::Instant>) -> contract::inbox::JobNotice {
    reported_with_usage_before(rig, horizon).0
}

/// [`reported_before`], also collecting the `DelegateUsage` deliveries
/// the watcher's tap forwards ahead of the runner's report: one scoped
/// clock driver and one shared take serve both.
#[track_caller]
fn reported_with_usage_before(
    rig: &Rig,
    horizon: Option<std::time::Instant>,
) -> (contract::inbox::JobNotice, Vec<(SessionId, String)>) {
    let stop = Arc::new(AtomicBool::new(false));
    let clock = Arc::clone(&rig.clock);
    let deadline = Deadline::after(DEADLINE);
    thread::scope(|scope| {
        scope.spawn(|| drive_runner_clock(&stop, &clock, &deadline, horizon));
        let mut usages = Vec::new();
        let notice = take_report(rig, &stop, "the runner did not report", &mut usages);
        (notice, usages)
    })
}

/// The loop advances only to a park the runner owns, and the drain wait
/// is recognised by distance, not by an exact bound: an ordinary poll
/// parks at most one interval ahead, so a park further ahead is the drain
/// wait. A scoped driver thread does the advancing; the test thread's
/// single receive is the wall-clock deadline for the whole wait, so a
/// runner that never reaches the drain fails the test instead of hanging
/// it. The `AFTER_DRAIN_DEADLINE` hook is a pause point between the drain
/// deadline's computation and its first park: it signals `reached`, then
/// blocks until `release`. On that signal the driver advances the clock
/// 1 s, then releases the hook, so the move lands between computation and
/// registration with the driver as the clock's only mover.
#[track_caller]
fn wake_until_draining(
    clock: &Arc<FakeClock>,
    _bound: Duration,
    reached: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
) {
    let stop = Arc::new(AtomicBool::new(false));
    let driver_stop = Arc::clone(&stop);
    let driver_clock = Arc::clone(clock);
    let deadline = Deadline::after(DEADLINE);
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(move || {
            let mut carried: Option<Instant> = None;
            loop {
                if driver_stop.load(Ordering::Relaxed) {
                    return;
                }
                if deadline.left().is_zero() {
                    panic!(
                        "the runner did not reach the drain wait; now={:?} parked={:?}",
                        driver_clock.now(),
                        driver_clock.parked()
                    );
                }
                // Each pass first checks the pause-point signal. One thread
                // moves the clock, so this raw move and the helper move
                // below never overlap.
                if reached.try_recv().is_ok() {
                    // Held runner parks nowhere; testing.md "Waits and timeouts" binds helper advances to a named park, so this raw advance is the one move made while the runner holds at the pause point.
                    driver_clock.advance(Duration::from_secs(1));
                    let _sent = release.send(());
                    continue;
                }
                let mut candidate = carried.take();
                if candidate.is_some_and(|watched| watched <= driver_clock.now()) {
                    candidate = None;
                }
                match candidate.or_else(|| park_ahead(&driver_clock)) {
                    Some(until) => match until.checked_duration_since(driver_clock.now()) {
                        Some(gap) if gap > super::POLL => {
                            let _sent = done_tx.send(());
                            return;
                        }
                        Some(gap) => match try_step(&driver_clock, until, gap) {
                            Step::Moved | Step::NoPark => {}
                            Step::Next(watched) => {
                                carried = Some(watched);
                            }
                        },
                        None => thread::yield_now(),
                    },
                    None => thread::yield_now(),
                }
            }
        });
        match deadline.recv(&done_rx) {
            Ok(()) => {
                stop.store(true, Ordering::Relaxed);
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                panic!(
                    "the runner did not reach the drain wait; now={:?} parked={:?}",
                    clock.now(),
                    clock.parked()
                );
            }
        }
    });
}

/// A FIFO `shell` blocks reading until the test writes. Short-lived and
/// stdio-null, so it holds no harness pipe even briefly.
fn fifo(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fakes::fifo(&path);
    path
}

/// Releases a child blocked reading `fifo`, without ever blocking: a
/// non-blocking open pairs with a waiting reader and fails fast with no
/// reader at all, so a write to a dead child's fifo fails the test
/// instead of hanging it. A starting child may not have opened yet; the
/// retries absorb that within a wall-clock bound.
#[track_caller]
fn release_fifo(fifo: &std::path::Path, bytes: &[u8]) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let fifo = fifo.to_path_buf();
    let bytes = bytes.to_vec();
    within(
        "a reader opens the fifo",
        Deadline::after(DEADLINE),
        move || {
            loop {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed())
                    .open(&fifo)
                {
                    Ok(mut fifo) => {
                        fifo.write_all(&bytes)
                            .expect("a few bytes always fit the pipe");
                        return;
                    }
                    Err(source)
                        if source.raw_os_error()
                            == Some(rustix::io::Errno::NXIO.raw_os_error()) =>
                    {
                        thread::yield_now();
                    }
                    Err(source) => panic!("opening {fifo:?} for writing failed: {source}"),
                }
            }
        },
    );
}

/// A pid `shell` wrote to `pid`: the leader's, or a member's.
#[track_caller]
fn pid_in_file(pid: &std::path::Path) -> u32 {
    let pid = pid.to_path_buf();
    within(
        "the leader writes its pid",
        Deadline::after(DEADLINE),
        move || {
            loop {
                let Ok(text) = std::fs::read_to_string(&pid) else {
                    thread::yield_now();
                    continue;
                };
                let Ok(pid) = text.trim().parse() else {
                    thread::yield_now();
                    continue;
                };
                return pid;
            }
        },
    )
}

/// Waits until the fake watch ran `n` times, under one wall-clock
/// deadline that only guards against a hang. It never moves the clock:
/// a caller that needs time to pass advances it itself.
fn wait_calls(rig: &Rig, n: usize) {
    assert!(
        rig.script.await_calls(n, DEADLINE),
        "the watch was not called {n} times"
    );
}

/// Waits until the scripted watch folded `n` deliveries: each `Closed`
/// or `Exited` call sends after `on_line` returned, so the fold already
/// ran. One wall-clock deadline covers all receives, so a missing fold
/// fails the test instead of hanging it.
#[track_caller]
fn wait_folded(rig: &Rig, n: usize) {
    let deadline = Deadline::after(DEADLINE);
    let folded = rig.script.folded_rx.lock().unwrap();
    for _ in 0..n {
        deadline.recv_or_fail(&folded, "the watch to fold its lines");
    }
}

/// Wakes the runner through one refused-watch backoff: the runner parks
/// at `at`, the test advances `gap`, and the watch runs again
/// (docs/testing.md, "Waits and timeouts": a test advances a fake clock
/// only after a signal that the code under test is waiting on that
/// clock). The caller counts `at` cumulatively from the first call, with
/// backoff 50, 100, 200, 400, 800 ms, then 1 s.
fn wake_retry(rig: &Rig, at: std::time::Instant, gap: Duration, calls: usize) {
    assert!(
        rig.clock.await_parked(at, fakes::Deadline::after(DEADLINE)),
        "the runner parks until {at:?}"
    );
    rig.clock
        .advance_when_parked(Some(at), None, gap, &Deadline::after(DEADLINE));
    wait_calls(rig, calls);
}

/// Waits until `shell` writes its readiness file: its traps are armed, so
/// a stop from here cannot land before them.
#[track_caller]
fn wait_ready(path: &std::path::Path) {
    let path = path.to_path_buf();
    within(
        "the child arms its traps",
        Deadline::after(DEADLINE),
        move || {
            loop {
                if std::fs::read_to_string(&path).is_ok_and(|text| !text.trim().is_empty()) {
                    return;
                }
                thread::yield_now();
            }
        },
    );
}

/// Waits, driving the fake clock, until `pgid` leaves the jobs list:
/// retirement runs on wakes. One wall-clock deadline covers the whole
/// wait (docs/testing.md, "Waits and timeouts"): the wait fails naming
/// the pgid instead of hanging. Each advance goes only to the retire
/// park's own `until` (docs/testing.md, "Waits and timeouts": a test
/// advances a fake clock only after a signal that the code under test is
/// waiting on that clock); with no park ahead the wait yields.
fn wait_retired(clock: &Arc<FakeClock>, pgid: u32) {
    let deadline = Deadline::after(DEADLINE);
    let mut carried: Option<Instant> = None;
    while listed(pgid) {
        if deadline.left().is_zero() {
            panic!(
                "pgid {pgid} retires; now={:?} parked={:?}",
                clock.now(),
                clock.parked()
            );
        }
        let mut candidate = carried.take();
        if candidate.is_some_and(|watched| watched <= clock.now()) {
            candidate = None;
        }
        match candidate.or_else(|| park_ahead(clock)) {
            Some(until) => match until.checked_duration_since(clock.now()) {
                Some(gap) if gap <= super::POLL => match try_step(clock, until, gap) {
                    Step::Moved | Step::NoPark => {}
                    Step::Next(watched) => {
                        carried = Some(watched);
                    }
                },
                _ => thread::yield_now(),
            },
            None => thread::yield_now(),
        }
    }
}

/// How many SIGKILLs went to `pgid` so far.
fn kills(pgid: u32) -> usize {
    crate::delegate::group::sent_signals()
        .iter()
        .filter(|(signalled, signal)| {
            *signalled == pgid && *signal == rustix::process::Signal::KILL
        })
        .count()
}

#[test]
fn the_watch_flow_completes_with_the_delegate_text() {
    // The child prints the same line the watch delivers: whether the reap
    // or the watch wins the race, the fold sees one `fiber_exited`.
    let line = json_line(2, "Done.");
    let rig = rig(vec![WatchReply::Exited(vec![
        envelope(1, "turn_completed", serde_json::json!({})),
        fiber_exited_line(2, "Done."),
    ])]);
    // Backstop armed before the shell starts: the child exits on
    // its own in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-delegate-watch-flow");
    rig.start(
        &format!(": fiber-delegate-watch-flow; printf '%s\\n' '{line}'; exit 0"),
        BOUND,
        1024,
    );
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Done.".into())
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_pre_session_error_fails_with_that_error() {
    // The production startup shape: `PreSessionExit` has no session
    // envelope around it, and carries no usage or final message. The
    // runner must preserve the child's own failure, not report a missing
    // result.
    let line = serde_json::to_string(&PreSessionExit::new(
        1,
        Failure {
            code: ErrorCode::InvalidArguments,
            message: "Bad model.".to_owned(),
            retry_after_ms: None,
            provider: None,
        },
    ))
    .unwrap();
    // Every watch is refused: the child failed before it could bind.
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: the child exits on
    // its own in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-delegate-presession");
    rig.start(
        &format!(": fiber-delegate-presession; printf '%s\\n' '{line}'; exit 1"),
        BOUND,
        1024,
    );
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.message.clone()),
        Some("Bad model.".to_owned())
    );
    assert_eq!(
        notice
            .completed
            .process
            .as_ref()
            .and_then(|process| process.exit_code),
        Some(1)
    );
    // No final message was ever written: the finish carries no text.
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("".into())
    );
    // No model call ran before the failure: zero usage, not a copy of
    // some earlier run's totals.
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.usage.clone()),
        Some(Usage::default())
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_full_envelope_error_keeps_its_usage_and_final_message() {
    // The normal session shape keeps everything it wrote: a startup
    // normalization must never erase a real envelope's usage or text.
    let errored = FiberExited {
        exit_code: 1,
        usage: usage(),
        final_message: Some(FinalMessage {
            final_action_id: ActionId("a_1".into()),
            text: "almost".to_owned(),
        }),
        error: Some(Failure {
            code: ErrorCode::InvalidArguments,
            message: "Bad model.".to_owned(),
            retry_after_ms: None,
            provider: None,
        }),
        suspended_on: None,
        questions: None,
    };
    let line = serde_json::to_string(&envelope(
        1,
        "fiber_exited",
        serde_json::to_value(&errored).unwrap(),
    ))
    .unwrap();
    // Every watch is refused: the result must come through stdout.
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: the child exits on
    // its own in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-delegate-full-envelope");
    rig.start(
        &format!(": fiber-delegate-full-envelope; printf '%s\\n' '{line}'; exit 1"),
        BOUND,
        1024,
    );
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("almost".into())
    );
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.usage.clone()),
        Some(usage())
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

/// Runs a child that prints `line` on stdout and exits `code` with
/// every watch refused, and returns its report: stdout is the only
/// verdict source, as for a child that failed before it could bind.
/// `marker` names the child, one per test, for its watchdog.
fn stdout_report(line: &str, code: i32, marker: &str) -> contract::inbox::JobNotice {
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: the child exits on
    // its own in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching(marker);
    rig.start(
        &format!(": {marker}; printf '%s\\n' '{line}'; exit {code}"),
        BOUND,
        1024,
    );
    let notice = reported(&rig);
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    notice
}

/// The report carries no verdict: the job ends without a result, and
/// keeps the process's own exit code.
fn assert_no_verdict(notice: &contract::inbox::JobNotice, code: i32) {
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::Indeterminate)
    );
    assert_eq!(
        notice
            .completed
            .process
            .as_ref()
            .and_then(|process| process.exit_code),
        Some(code)
    );
}

#[test]
fn a_session_line_of_another_kind_is_no_verdict() {
    let line =
        serde_json::to_string(&envelope(1, "turn_completed", serde_json::json!({}))).unwrap();
    let notice = stdout_report(&line, 1, "fiber-delegate-wrong-kind");
    assert_no_verdict(&notice, 1);
}

#[test]
fn a_non_object_stdout_line_is_no_verdict() {
    let notice = stdout_report("[1, 2, 3]", 1, "fiber-delegate-non-object");
    assert_no_verdict(&notice, 1);
}

#[test]
fn a_startup_line_without_its_error_is_no_verdict() {
    let notice = stdout_report(
        r#"{"kind":"fiber_exited","schema_version":1,"payload":{"exit_code":1}}"#,
        1,
        "fiber-delegate-no-error",
    );
    assert_no_verdict(&notice, 1);
}

#[test]
fn a_startup_line_without_its_exit_code_is_no_verdict() {
    let notice = stdout_report(
        r#"{"kind":"fiber_exited","schema_version":1,"payload":{"error":{"code":"invalid_arguments","message":"Bad model."}}}"#,
        1,
        "fiber-delegate-no-exit-code",
    );
    assert_no_verdict(&notice, 1);
}

#[test]
fn a_startup_line_with_a_string_exit_code_is_no_verdict() {
    let notice = stdout_report(
        r#"{"kind":"fiber_exited","schema_version":1,"payload":{"error":{"code":"invalid_arguments","message":"Bad model."},"exit_code":"1"}}"#,
        1,
        "fiber-delegate-string-exit-code",
    );
    assert_no_verdict(&notice, 1);
}

#[test]
fn a_startup_line_without_a_schema_version_is_no_verdict() {
    let notice = stdout_report(
        r#"{"kind":"fiber_exited","payload":{"error":{"code":"invalid_arguments","message":"Bad model."},"exit_code":1}}"#,
        1,
        "fiber-delegate-no-schema-version",
    );
    assert_no_verdict(&notice, 1);
}

#[test]
fn a_startup_line_of_another_kind_is_no_verdict() {
    let notice = stdout_report(
        r#"{"kind":"fiber_started","schema_version":1,"payload":{"error":{"code":"invalid_arguments","message":"Bad model."},"exit_code":1}}"#,
        1,
        "fiber-delegate-startup-wrong-kind",
    );
    assert_no_verdict(&notice, 1);
}

#[test]
fn a_session_envelope_carrying_a_startup_payload_is_no_verdict() {
    // The same payload succeeds with no session identity, but a line
    // carrying one is judged as a session envelope or not at all: the
    // session shape requires usage the startup shape never writes, so
    // this is no verdict rather than a laundered startup error.
    let line = serde_json::to_string(&envelope(
        1,
        "fiber_exited",
        serde_json::json!({"error": {"code": "invalid_arguments", "message": "Bad model."}, "exit_code": 1}),
    ))
    .unwrap();
    let notice = stdout_report(&line, 1, "fiber-delegate-envelope-startup");
    assert_no_verdict(&notice, 1);
}

#[test]
fn an_unrelated_object_line_is_no_verdict() {
    let notice = stdout_report(r#"{"hello":"world"}"#, 1, "fiber-delegate-unrelated");
    assert_no_verdict(&notice, 1);
}

#[test]
fn a_startup_line_with_an_additive_field_still_fails() {
    // Unknown fields ride along: the startup verdict survives them.
    let notice = stdout_report(
        r#"{"kind":"fiber_exited","schema_version":1,"note":"added later","payload":{"error":{"code":"invalid_arguments","message":"Bad model."},"exit_code":1}}"#,
        1,
        "fiber-delegate-additive-field",
    );
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.message.clone()),
        Some("Bad model.".to_owned())
    );
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("".into())
    );
}

#[test]
fn the_last_stdout_verdict_wins() {
    let first = serde_json::to_string(&PreSessionExit::new(
        1,
        Failure {
            code: ErrorCode::InvalidArguments,
            message: "First.".to_owned(),
            retry_after_ms: None,
            provider: None,
        },
    ))
    .unwrap();
    let second = serde_json::to_string(&PreSessionExit::new(
        1,
        Failure {
            code: ErrorCode::InvalidArguments,
            message: "Second.".to_owned(),
            retry_after_ms: None,
            provider: None,
        },
    ))
    .unwrap();
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: the child exits on
    // its own in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-delegate-last-wins");
    rig.start(
        &format!(
            ": fiber-delegate-last-wins; printf '%s\\n' '{first}'; printf '%s\\n' '{second}'; exit 1"
        ),
        BOUND,
        1024,
    );
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.message.clone()),
        Some("Second.".to_owned())
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_malformed_stdout_line_ends_indeterminate() {
    // Garbage on stdout is no verdict at all: the job ends without a
    // result, and keeps the process's own exit code.
    assert_no_verdict(
        &stdout_report("not json at all{{{", 1, "fiber-delegate-malformed"),
        1,
    );
}

#[test]
fn a_child_that_exits_before_any_connect_uses_stdout() {
    let line = json_line(1, "Quick.");
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: the child exits on
    // its own in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-delegate-early-exit");
    rig.start(
        &format!(": fiber-delegate-early-exit; printf '%s\\n' '{line}'; exit 0"),
        BOUND,
        1024,
    );
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Quick.".into())
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_delayed_drain_still_feeds_the_fold() {
    let dir = TempDir::new("fiber-delegate-delayed");
    let release = fifo(dir.path(), "release");
    let ready_fifo = fifo(dir.path(), "ready");
    // The member leaves the leader's group before it blocks: the reap's
    // SIGKILL cannot reach it, and the pipe stays open until it prints.
    // The leader leaves only after the member is set up, so the pipe is
    // never unheld. Its command line carries the fifo path, which the
    // watchdog below matches as a backstop.
    let line = json_line(7, "Late.");
    let shell = format!(
        "perl -MPOSIX -e 'POSIX::setsid(); open(my $W, \">\", $ARGV[2]) or die $!; print $W \"ok\\n\"; close $W; open(my $F, \"<\", $ARGV[0]) or die $!; my $x = <$F>; print STDOUT \"$ARGV[1]\\n\";' '{}' '{line}' '{}' & read _ < '{}'; exit 0",
        release.display(),
        ready_fifo.display(),
        ready_fifo.display()
    );
    let watchdog = Watchdog::matching(&release.to_string_lossy());
    // The watch never connects: only the drain can carry the line.
    let rig = rig(vec![]);
    // A pause point between the drain deadline's computation and its first
    // park: the hook signals it has been reached, then blocks until the
    // driver releases it. The driver advances the clock 1 s, then releases
    // the hook, so the move lands between computation and registration with
    // the driver as the clock's only mover. Both channels exist before the
    // hook is installed, so before
    // `rig.start` starts the runner that runs it. The release wait fails
    // closed, like the pause points in `fakes::clock`: if the driver is
    // gone without releasing, the runner panics instead of parking past
    // the point unreleased.
    let (reached_tx, reached_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    super::lock(&super::AFTER_DRAIN_DEADLINE).replace(Box::new(move || {
        let _sent = reached_tx.send(());
        Deadline::after(Duration::from_secs(5))
            .recv(&release_rx)
            .expect("waited for the driver to release the drain pause point");
    }));
    rig.start(&shell, Duration::from_secs(30), 1024);
    // The runner is waiting for the drain once it is parked at the stop
    // bound. Each wake is given only after the runner has parked on the
    // clock, so the reap lands on a wake it is waiting for; the clock stays
    // still once the bound park is seen, so only the drain's end can wake it.
    wake_until_draining(&rig.clock, Duration::from_secs(30), reached_rx, release_tx);
    assert!(
        rig.inbox.try_recv().is_err(),
        "the fold waits for the drain"
    );
    release_fifo(&release, b"go\n");
    let Ok(Delivery::Job(notice)) = Deadline::after(DEADLINE).recv(&rig.inbox) else {
        panic!("the drain's end did not wake the runner");
    };
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Late.".into())
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_member_holding_stdout_past_the_reap_ends_indeterminate() {
    let dir = TempDir::new("fiber-delegate-held-stdout");
    let member_pid = dir.path().join("member");
    let ready = fifo(dir.path(), "ready");
    // Escaped past the leader's group, the sleeper holds the pipe past
    // the reap: the fold proceeds after the bound with no `fiber_exited`,
    // and does not hang. The leader leaves only after the member writes
    // past its own `setsid`, so the reap's SIGKILL cannot reach it: an
    // early leader exit would reap and signal the group before the
    // member escapes, closing the pipe and ending the drain early.
    let shell = format!(
        "perl -MPOSIX -e 'POSIX::setsid(); open(my $W, \">\", $ARGV[0]) or die $!; print $W \"ok\\n\"; close $W; sleep 60; # fiber-delegate-held-stdout' '{}' & echo $! > '{}'; read _ < '{}'; exit 0",
        ready.display(),
        member_pid.display(),
        ready.display()
    );
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: on any early failure the
    // Drop kills the escaped sleeper, which would otherwise hold stdout
    // for 60 s.
    let watchdog = Watchdog::matching("fiber-delegate-held-stdout");
    rig.start(&shell, BOUND, 1024);
    let member = pid_in_file(&member_pid);
    // The runner waits for the drain past the poll horizon: the only
    // test that needs fake time past the drain wait. A scoped driver
    // walks it there, then makes one advance to the drain park's own
    // `until`; the single receive below is the one wall-clock deadline
    // for the drain wait and the report together (docs/testing.md,
    // "Waits and timeouts": one deadline for the whole wait). The stop
    // flag is set on every outcome of that receive, before the result
    // is inspected, so the driver always ends within `DEADLINE` and the
    // scope never outlives the wait.
    let stop = Arc::new(AtomicBool::new(false));
    let driver_stop = Arc::clone(&stop);
    let clock = Arc::clone(&rig.clock);
    let deadline = Deadline::after(DEADLINE);
    let notice = thread::scope(|scope| {
        scope.spawn(move || {
            let mut carried: Option<Instant> = None;
            loop {
                if driver_stop.load(Ordering::Relaxed) {
                    return;
                }
                if deadline.left().is_zero() {
                    panic!(
                        "the drain's end did not report; now={:?} parked={:?}",
                        clock.now(),
                        clock.parked()
                    );
                }
                // The carried instant stands in for the park read this
                // pass; otherwise the finite instants ahead decide. With
                // zero or several, the driver yields as before.
                let now = clock.now();
                let mut candidate = carried.take();
                if candidate.is_some_and(|watched| watched <= now) {
                    candidate = None;
                }
                let ahead: Vec<Instant> = match candidate {
                    Some(watched) => vec![watched],
                    None => clock
                        .parked()
                        .iter()
                        .flatten()
                        .filter(|until| **until > now)
                        .copied()
                        .collect(),
                };
                if ahead.len() != 1 {
                    thread::yield_now();
                    continue;
                }
                // At the drain wait, one advance to its own `until`
                // ends the driving; the report follows on the test
                // thread's receive.
                let until = ahead[0];
                let advance = until - now;
                match try_step(&clock, until, advance) {
                    Step::Moved if until >= now + BOUND => return,
                    Step::Moved | Step::NoPark => {}
                    Step::Next(watched) => {
                        carried = Some(watched);
                    }
                }
            }
        });
        take_report(
            &rig,
            &stop,
            "the drain's end did not report",
            &mut Vec::new(),
        )
    });
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::Indeterminate)
    );
    kill_pid(member, "KILL").unwrap();
    assert!(
        pids_exit(&[member], fakes::Deadline::after(DEADLINE)),
        "the escaped member is reaped"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn exit_zero_with_no_line_anywhere_is_indeterminate() {
    let rig = rig(vec![]);
    // Backstop armed before the shell starts: the child exits on its own
    // in milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-delegate-exit-zero");
    rig.start(": fiber-delegate-exit-zero; exit 0", BOUND, 1024);
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::Indeterminate)
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_stalled_startup_backs_off_to_one_second_then_flows() {
    let dir = TempDir::new("fiber-delegate-backoff");
    let fifo = fifo(dir.path(), "release");
    let pidfile = dir.path().join("pid");
    let line = json_line(10, "Bound.");
    let shell = format!(
        "echo $$ > '{}'; read _ < '{}'; printf '%s\\n' '{line}'; exit 0",
        pidfile.display(),
        fifo.display()
    );
    // Each refused call is held until the test releases it, so the runner's
    // park on the poll horizon while it runs is told apart from its park
    // on the retry: once the backoff caps at the poll, both share a
    // deadline.
    let (gates, mut replies): (Vec<_>, Vec<_>) = (0..7)
        .map(|_| {
            let (gate, held) = mpsc::channel();
            (gate, WatchReply::Block(held))
        })
        .unzip();
    replies.push(WatchReply::Exited(vec![
        envelope(9, "turn_completed", serde_json::json!({})),
        fiber_exited_line(10, "Bound."),
    ]));
    let rig = rig(replies);
    let done = rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_calls(&rig, 1);
    // 50, 100, 200, 400, 800 ms, then capped at 1 s, never more often.
    let mut at = rig.script.calls()[0];
    for (held, (gate, gap)) in gates
        .iter()
        .zip([50, 100, 200, 400, 800, 1000, 1000])
        .enumerate()
    {
        // The call is held: the runner parks on the poll horizon. Its
        // next park, once it has taken the refusal, is the retry.
        let running = rig
            .clock
            .mark_parked(at + super::POLL, fakes::Deadline::after(DEADLINE))
            .expect("the runner parks while the watch runs");
        gate.send(()).expect("the held watch takes its release");
        at += Duration::from_millis(gap);
        assert!(
            rig.clock
                .await_parked_since(&running, Some(at), fakes::Deadline::after(DEADLINE)),
            "the runner parks until {at:?}"
        );
        rig.clock.advance_when_parked(
            Some(at),
            None,
            Duration::from_millis(gap),
            &Deadline::after(DEADLINE),
        );
        wait_calls(&rig, held + 2);
    }
    // The eighth call succeeds; the child binds and its lines flow.
    // The call returns before the watch folds the envelope: releasing
    // here lets the reap win, so wait for the fold first.
    wait_folded(&rig, 1);
    release_fifo(&fifo, b"go\n");
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        rig.script.calls().len(),
        8,
        "seven retries and the first call, then nothing more"
    );
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Bound.".into())
    );
    // Waiting for the runner to return rules out later watches and timer signals.
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the runner returns after `fiber_exited`");
    assert_eq!(
        rig.script.calls().len(),
        8,
        "after `fiber_exited` the watch rests"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_closed_watch_is_retried_and_each_seq_counts_once() {
    let dir = TempDir::new("fiber-delegate-closed");
    let fifo = fifo(dir.path(), "release");
    let shell = format!("read _ < '{}'; exit 0", fifo.display());
    // The fold signal, sent by the watcher thread once it stores each
    // result: the test waits on it instead of sleeping.
    let (fold_tx, fold_rx) = mpsc::channel();
    super::lock(&super::ON_WATCH_FOLD).replace(Box::new(move || {
        let _sent = fold_tx.send(());
    }));
    // The first watch blocks until the test releases it, so its `Retry`
    // folds while the runner is paused between its result take and its
    // launch decision: the launch block, not the take above, advances the
    // backoff. Dropping the sender releases it too, so a failed test
    // never leaves the watcher parked.
    let (gate_tx, gate_rx) = mpsc::channel();
    let rig = rig(vec![
        WatchReply::Block(gate_rx),
        WatchReply::Closed(vec![envelope(1, "turn_completed", serde_json::json!({}))]),
        WatchReply::Closed(vec![
            envelope(1, "turn_completed", serde_json::json!({})),
            envelope(2, "turn_completed", serde_json::json!({})),
        ]),
        WatchReply::Exited(vec![fiber_exited_line(3, "Replayed.")]),
    ]);
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&fifo.to_string_lossy());
    let done = rig.start(&shell, BOUND, 1024);
    wait_calls(&rig, 1);
    // One-shot pause between the result take and the launch decision.
    let (reached_tx, reached_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    super::lock(&super::AFTER_TAKE_RESULT).replace(Box::new(move || {
        let _sent = reached_tx.send(());
        let _released = go_rx.recv();
    }));
    // Wakes the parked runner into the pause: the clock moves only to the
    // runner's own park, and the take above the pause saw nothing, because
    // the watch is still gated.
    let at = rig.script.calls()[0];
    rig.clock.advance_when_parked(
        Some(at + super::POLL),
        None,
        super::POLL,
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE)
        .recv(&reached_rx)
        .expect("the runner pauses between the take and the launch");
    // Releases the watch, waits for the `Retry` to fold inside the pause,
    // then resumes the runner to its launch decision: the launch block
    // backs the retry off 50 ms and doubles to 100 ms.
    gate_tx
        .send(())
        .expect("the blocked watch takes its release");
    Deadline::after(DEADLINE)
        .recv(&fold_rx)
        .expect("the watcher folds its result");
    super::lock(&super::ON_WATCH_FOLD).take();
    go_tx.send(()).expect("the runner resumes to the launch");
    // The child waits for the fourth delivery before it may exit: the
    // replay above cannot win the race with the reap. Each retry backs
    // off 50 ms, then 100 ms, then 200 ms. The second gap proves the
    // launch block doubled the backoff: halving it would park 25 ms out
    // instead.
    let mut at = rig.clock.now();
    for (gap, calls) in [(50, 2), (100, 3), (200, 4)] {
        at += Duration::from_millis(gap);
        assert!(
            rig.clock.await_parked(at, fakes::Deadline::after(DEADLINE)),
            "the runner parks until {at:?}"
        );
        rig.clock.advance_when_parked(
            Some(at),
            None,
            Duration::from_millis(gap),
            &Deadline::after(DEADLINE),
        );
        wait_calls(&rig, calls);
    }
    // The fourth call returns before the watch folds the envelope:
    // releasing here lets the reap win, so wait for all three folds (the
    // blocked first call folds none).
    wait_folded(&rig, 3);
    release_fifo(&fifo, b"go\n");
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(rig.script.calls().len(), 4);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Replayed.".into())
    );
    // Waiting for the runner to return rules out later watches and timer signals.
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the runner returns after `fiber_exited`");
    assert_eq!(
        rig.script.calls().len(),
        4,
        "after `fiber_exited` the watch rests"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn the_watch_is_not_called_again_after_fiber_exited() {
    let dir = TempDir::new("fiber-delegate-rests");
    let fifo = fifo(dir.path(), "release");
    let shell = format!("read _ < '{}'; exit 0", fifo.display());
    // The fold signal, sent by the watcher thread once it stores each
    // result: the test waits on it instead of sleeping.
    let (fold_tx, fold_rx) = mpsc::channel();
    super::lock(&super::ON_WATCH_FOLD).replace(Box::new(move || {
        let _sent = fold_tx.send(());
    }));
    // The watch delivers its `fiber_exited` line, then blocks until the
    // test releases it: the fold cannot land before the runner pauses
    // below, so the ordering is forced on every run.
    let (gate_tx, gate_rx) = mpsc::channel();
    let rig = rig(vec![WatchReply::Gated(
        vec![fiber_exited_line(1, "Once.")],
        gate_rx,
    )]);
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&fifo.to_string_lossy());
    let done = rig.start(&shell, BOUND, 1024);
    wait_calls(&rig, 1);
    // One-shot pause between the result take and the launch decision.
    let (reached_tx, reached_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    super::lock(&super::AFTER_TAKE_RESULT).replace(Box::new(move || {
        let _sent = reached_tx.send(());
        let _released = go_rx.recv();
    }));
    // Wakes the parked runner into the pause: the clock moves only to the
    // runner's own park, and the take above the pause saw nothing, because
    // the watch is still gated.
    let at = rig.script.calls()[0];
    rig.clock.advance_when_parked(
        Some(at + super::POLL),
        None,
        super::POLL,
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE)
        .recv(&reached_rx)
        .expect("the runner pauses between the take and the launch");
    // Releases the watch, waits for the fold to land inside the pause,
    // then resumes the runner to its launch decision.
    gate_tx.send(()).expect("the gated watch takes its release");
    Deadline::after(DEADLINE)
        .recv(&fold_rx)
        .expect("the watcher folds its result");
    super::lock(&super::ON_WATCH_FOLD).take();
    // A second one-shot pause on the next take: the runner is still held
    // in the first pause, so this is installed before the launch decision
    // below runs. The next take proves the decision already ran: without
    // the fix the runner launched a second watch from its stale take,
    // with it the runner launches none. Only then may the child exit:
    // releasing the fifo any earlier lets the reap return first, and the
    // red run passes with one call.
    let (reached2_tx, reached2_rx) = mpsc::channel();
    let (go2_tx, go2_rx) = mpsc::channel();
    super::lock(&super::AFTER_TAKE_RESULT).replace(Box::new(move || {
        let _sent = reached2_tx.send(());
        let _released = go2_rx.recv();
    }));
    go_tx.send(()).expect("the runner resumes to the launch");
    Deadline::after(DEADLINE)
        .recv(&reached2_rx)
        .expect("the runner takes again after the launch decision");
    go2_tx
        .send(())
        .expect("the runner resumes past the second take");
    release_fifo(&fifo, b"go\n");
    // Fake time never reaches the horizon, so a refused retry's backoff
    // cannot expire into another watch before the reap.
    let notice = reported_before(&rig, Some(rig.clock.now()));
    assert_eq!(notice.completed.status, Outcome::Completed);
    // Waiting for the runner to return rules out later watches and timer signals.
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the runner returns after `fiber_exited`");
    assert_eq!(rig.script.calls().len(), 1);
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn timeout_boundaries_and_parks() {
    let clock = FakeClock::new();
    let d = clock.now() + Duration::from_secs(1);
    assert!(!super::deadline_due(None, d));
    assert!(!super::deadline_due(
        Some(d),
        d.checked_sub(Duration::from_nanos(1)).unwrap()
    ));
    assert!(super::deadline_due(Some(d), d));
    assert!(super::deadline_due(Some(d), d + Duration::from_nanos(1)));
    assert_eq!(super::park_due(d + BOUND, true, false, d, None, Some(d)), d);
    assert_eq!(
        super::park_due(d + BOUND, true, false, d, None, None),
        d + BOUND
    );
}

#[test]
fn timeout_keeps_the_first_termination_reason() {
    for (first, second) in [
        (Termination::Stopped, Termination::Timeout),
        (Termination::Timeout, Termination::Stopped),
        (Termination::OutputCap, Termination::Timeout),
    ] {
        let clock = FakeClock::new();
        let shared = super::Shared::new(clock, BOUND);
        shared.halt(first);
        shared.halt(second);
        assert_eq!(shared.reason(), Some(first));
    }
}

#[test]
fn timeout_stops_a_term_trap_without_sigkill() {
    let dir = TempDir::new("fiber-delegate-timeout");
    let pidfile = dir.path().join("pid");
    let ready = dir.path().join("ready");
    let fifo = fifo(dir.path(), "block");
    let shell = format!(
        "echo $$ > '{}'; trap 'exit 143' TERM; echo ready > '{}'; read _ < '{}'",
        pidfile.display(),
        ready.display(),
        fifo.display()
    );
    let (_release, held) = mpsc::channel();
    let rig = rig(vec![WatchReply::Block(held)]);
    let origin = rig.clock.now();
    let timeout = Duration::from_millis(500);
    let (done, park) = rig.start_timeout(&shell, BOUND, 1024, None, Some(timeout), None);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    let deadline = Deadline::after(DEADLINE);
    rig.clock
        .advance_when_parked(Some(origin + timeout), None, timeout, &deadline);
    // Wake subsequent polls without moving beyond the stop bound.
    let notice = timeout_report(&rig, &park);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice.completed.error.unwrap().code,
        contract::ErrorCode::Timeout
    );
    let process = notice.completed.process.unwrap();
    assert!(process.timed_out);
    assert_eq!(process.exit_code, Some(143));
    deadline.recv(&done).expect("timeout runner returned");
    assert!(
        !crate::delegate::group::sent_signals().contains(&(pgid, rustix::process::Signal::KILL))
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn timeout_does_not_replace_a_childs_own_early_exit() {
    let dir = TempDir::new("fiber-delegate-before-timeout");
    let pidfile = dir.path().join("pid");
    let block = fifo(dir.path(), "block");
    let rig = rig(vec![]);
    let shell = format!(
        "echo $$ > '{}'; read _ < '{}'; printf '%s\\n' '{}'",
        pidfile.display(),
        block.display(),
        json_line(1, "done")
    );
    let (done, park) = rig.start_timeout(&shell, BOUND, 1024, None, Some(BOUND * 10), None);
    let watchdog = Watchdog::group(pid_in_file(&pidfile));
    release_fifo(&block, b"go\n");
    let notice = timeout_report(&rig, &park);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert!(!notice.completed.process.unwrap().timed_out);
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("early runner returns");
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

fn timeout_report(rig: &Rig, park: &Parker) -> contract::inbox::JobNotice {
    use contract::clock::Wake as _;
    let stopped = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            // Spurious wakes exercise the reap without advancing the deadline or stop bound.
            while !stopped.load(Ordering::Relaxed) {
                park.wake();
                thread::yield_now();
            }
        });
        take_report(rig, &stopped, "timeout runner report", &mut Vec::new())
    })
}

#[test]
fn a_stop_on_a_term_trap_cancels_without_sigkill() {
    let dir = TempDir::new("fiber-delegate-trap");
    let pidfile = dir.path().join("pid");
    let ready = dir.path().join("ready");
    let fifo = fifo(dir.path(), "block");
    // A builtin `read` blocks with no child to race: TERM interrupts it
    // and the trap exits the shell itself.
    let shell = format!(
        "echo $$ > '{}'; trap 'exit 143' TERM; echo ready > '{}'; read _ < '{}'",
        pidfile.display(),
        ready.display(),
        fifo.display()
    );
    let rig = rig(vec![]);
    let done = rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    // Fake time never reaches the stop bound, so the trap's own exit
    // cannot race the timer's SIGKILL (docs/testing.md, "Waits and
    // timeouts": deadlines are hang guards, never timing assertions).
    let kill_at = rig.clock.now() + BOUND;
    assert_eq!(rig.registry.stop_delegates(), 1);
    let notice = reported_before(&rig, Some(kill_at));
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    assert_eq!(
        notice
            .completed
            .process
            .as_ref()
            .and_then(|process| process.exit_code),
        Some(143)
    );
    // Waiting for the runner to return rules out later watches and timer signals.
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the runner returns after the trap's exit");
    assert!(
        !crate::delegate::group::sent_signals()
            .iter()
            .any(|(signalled, signal)| *signalled == pgid
                && *signal == rustix::process::Signal::KILL),
        "no SIGKILL went to the reaped group"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_stop_on_a_term_ignorer_kills_past_the_bound() {
    let dir = TempDir::new("fiber-delegate-ignore");
    let pidfile = dir.path().join("pid");
    let ready = dir.path().join("ready");
    let fifo = fifo(dir.path(), "block");
    let shell = format!(
        "echo $$ > '{}'; trap '' TERM; echo ready > '{}'; read _ < '{}'",
        pidfile.display(),
        ready.display(),
        fifo.display()
    );
    let rig = rig(vec![]);
    rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    assert_eq!(rig.registry.stop_delegates(), 1);
    // No stepped advance: the driver inside `reported` walks the runner
    // through its retry parks to the park at the stop bound, whose
    // advance sends SIGKILL (docs/testing.md, "Waits and timeouts": a
    // test advances a fake clock only after a signal that the code under
    // test is waiting on that clock).
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    assert_eq!(
        notice
            .completed
            .process
            .as_ref()
            .and_then(|process| process.signal.clone()),
        Some("SIGKILL".to_owned())
    );
    assert!(
        crate::delegate::group::sent_signals()
            .iter()
            .any(|(signalled, signal)| *signalled == pgid
                && *signal == rustix::process::Signal::KILL),
        "SIGKILL went out once the bound passed"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn the_cap_at_an_exact_boundary_trips_only_past_it() {
    let dir = TempDir::new("fiber-delegate-cap");
    let fifo = fifo(dir.path(), "release");
    let shell = format!("read _ < '{}'; exit 0", fifo.display());
    let rig = rig(vec![
        WatchReply::Closed(vec![envelope(1, "turn_completed", serde_json::json!({}))]),
        WatchReply::Closed(vec![
            envelope(1, "turn_completed", serde_json::json!({})),
            envelope(2, "turn_completed", serde_json::json!({})),
        ]),
    ]);
    // Ten bytes: exactly at the cap. Neither the new line, its replay,
    // nor a backoff wake may stop the delegate for that. Each receive
    // below must time out: a report here means the cap tripped at the
    // boundary, and swallowing it would hang the fifo write below on a
    // dead child (docs/testing.md, "Waits and timeouts": a receive
    // with a bound, not a sleep).
    std::fs::write(&rig.events, "0123456789").unwrap();
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&fifo.to_string_lossy());
    rig.start(&shell, BOUND, 10);
    // Calls 1 and 2 are `Closed` with the new line and its replay;
    // calls 3 and 4 are refused backoff wakes.
    wait_calls(&rig, 1);
    let mut at = rig.script.calls()[0];
    for (gap, calls) in [(50, 2), (100, 3), (200, 4)] {
        at += Duration::from_millis(gap);
        wake_retry(&rig, at, Duration::from_millis(gap), calls);
        if Deadline::after(Duration::from_millis(20))
            .recv(&rig.inbox)
            .is_ok()
        {
            panic!("exactly at the cap is not past it");
        }
    }
    // The fourth call is refused, so the backoff is 400 ms.
    at += Duration::from_millis(400);
    assert!(
        rig.clock.await_parked(at, fakes::Deadline::after(DEADLINE)),
        "the runner parks until {at:?}"
    );
    if Deadline::after(Duration::from_millis(20))
        .recv(&rig.inbox)
        .is_ok()
    {
        panic!("exactly at the cap is not past it");
    }
    std::fs::write(&rig.events, "01234567890").unwrap();
    // Both `Closed` calls fold before the release: the reap must not
    // beat the fold.
    wait_folded(&rig, 2);
    release_fifo(&fifo, b"go\n");
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::OutputCap)
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_disconnected_cap_trips_on_a_backoff_wake() {
    let dir = TempDir::new("fiber-delegate-blind");
    let fifo = fifo(dir.path(), "release");
    let shell = format!(
        "for i in 1 2 3 4; do echo filler-line >> '{}'; done; read _ < '{}'",
        "EVENTS",
        fifo.display()
    );
    let rig = rig(vec![]);
    let shell = shell.replace("EVENTS", &rig.events.to_string_lossy());
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&fifo.to_string_lossy());
    rig.start(&shell, BOUND, 32);
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::OutputCap)
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_stop_before_the_cap_keeps_cancelled() {
    let dir = TempDir::new("fiber-delegate-first-wins");
    let fifo = fifo(dir.path(), "release");
    let ready = dir.path().join("ready");
    let shell = format!(
        "printf '0123456789' >> '{}'; trap '' TERM; echo ready > '{}'; read _ < '{}'",
        "EVENTS",
        ready.display(),
        fifo.display()
    );
    let rig = rig(vec![]);
    let shell = shell.replace("EVENTS", &rig.events.to_string_lossy());
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&fifo.to_string_lossy());
    rig.start(&shell, BOUND, 10);
    wait_ready(&ready);
    assert_eq!(rig.registry.stop_delegates(), 1);
    // The watch is refused every time, so from call 1 the runner parks
    // at 50, 150 and 350 ms on (backoff 50, 100, 200 ms): the runner
    // passes after the stop with the log exactly at the cap.
    wait_calls(&rig, 1);
    let mut at = rig.script.calls()[0];
    for (gap, calls) in [(50, 2), (100, 3), (200, 4)] {
        at += Duration::from_millis(gap);
        wake_retry(&rig, at, Duration::from_millis(gap), calls);
    }
    // Past the cap, but the stop was first: the end stays `cancelled`.
    std::fs::write(&rig.events, "01234567890").unwrap();
    release_fifo(&fifo, b"go\n");
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_child_that_exits_in_the_stop_timer_sends_no_kill() {
    let dir = TempDir::new("fiber-delegate-quick-stop");
    let pidfile = dir.path().join("pid");
    let ready = dir.path().join("ready");
    let fifo = fifo(dir.path(), "block");
    let shell = format!(
        "echo $$ > '{}'; trap 'exit 0' TERM; echo ready > '{}'; read _ < '{}'",
        pidfile.display(),
        ready.display(),
        fifo.display()
    );
    let rig = rig(vec![]);
    let done = rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    // Fake time never reaches the stop bound, so the quick exit cannot
    // race the timer's SIGKILL (docs/testing.md, "Waits and timeouts":
    // deadlines are hang guards, never timing assertions).
    let kill_at = rig.clock.now() + BOUND;
    assert_eq!(rig.registry.stop_delegates(), 1);
    let notice = reported_before(&rig, Some(kill_at));
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    // Waiting for the runner to return rules out later watches and timer signals.
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the runner returns after the quick exit");
    assert!(
        !crate::delegate::group::sent_signals()
            .iter()
            .any(|(signalled, signal)| *signalled == pgid
                && *signal == rustix::process::Signal::KILL),
        "the timer found the group retired"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_member_outliving_a_stop_is_killed_and_the_job_still_cancels() {
    let dir = TempDir::new("fiber-delegate-stop-member");
    let leader = dir.path().join("leader");
    let member = dir.path().join("member");
    let shell = format!(
        "echo $$ > '{}'; trap '' TERM; sleep 60 & echo $! > '{}'; trap 'exit 0' TERM; read _ < /dev/stdin",
        leader.display(),
        member.display()
    );
    let rig = rig(vec![]);
    rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&leader);
    let watchdog = Watchdog::group(pgid);
    let survivor: u32 = within("the member writes its pid", Deadline::after(DEADLINE), {
        let member = member.clone();
        move || loop {
            let Ok(text) = std::fs::read_to_string(&member) else {
                thread::yield_now();
                continue;
            };
            let Ok(pid) = text.trim().parse() else {
                thread::yield_now();
                continue;
            };
            return pid;
        }
    });
    assert_eq!(rig.registry.stop_delegates(), 1);
    let notice = reported(&rig);
    // The job ends as soon as the leader is reaped, not when the member
    // is gone.
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    assert!(
        pids_exit(&[survivor], fakes::Deadline::after(DEADLINE)),
        "the surviving member got SIGKILL"
    );
    assert!(
        group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the group retires once it is empty"
    );
    // Retired, not just empty, once the runner has run: without the
    // retire loop the pgid would stay listed after its members are gone.
    wait_retired(&rig.clock, pgid);
    assert!(!listed(pgid));
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_member_listed_past_the_bound_gets_sigkill_from_retire() {
    let dir = TempDir::new("fiber-delegate-retire-timer");
    let clock = FakeClock::new();
    // A live group with no delegate attached: only `retire` supervises
    // it, so every SIGKILL below is the retire timer's.
    let mut member = Command::new("sleep");
    member
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let (mut member, listing) = support::group::spawn(&mut member).unwrap();
    let pgid = member.id();
    let mut listing = Some(listing);
    let watchdog = Watchdog::group(pgid);
    let watch: Watch = Arc::new(
        |_: &SessionId, _: Option<u64>, _: &mut dyn FnMut(&Envelope)| {
            Err(std::io::Error::other("refused"))
        },
    );
    let (runner, stop) = Runner::new(
        crate::registry::mint_job_id(),
        mint_session_id(),
        dir.path().to_path_buf(),
        Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        BOUND,
        1024,
        watch,
        None,
        None,
        None,
    );
    // The stop sets the bound; the member ignores nothing and no other
    // thread signals this group.
    stop.0();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        runner.retire(&mut listing);
        let _sent = done_tx.send(());
    });
    let mut mark = clock.advance_when_parked(
        Some(clock.origin() + Duration::from_secs(1)),
        None,
        super::POLL,
        &Deadline::after(DEADLINE),
    );
    for k in 2..=BOUND.as_secs() {
        mark = clock.advance_when_parked(
            Some(clock.origin() + Duration::from_secs(k)),
            None,
            super::POLL,
            &Deadline::after(DEADLINE),
        );
    }
    let after_bound = clock.origin() + BOUND + Duration::from_secs(1);
    assert!(
        clock.await_parked_since(&mark, Some(after_bound), fakes::Deadline::after(DEADLINE)),
        "retire checks the timer and parks again past the bound"
    );
    assert!(kills(pgid) >= 1, "SIGKILL went out past the bound");

    let (reaped_tx, reaped_rx) = mpsc::channel();
    thread::spawn(move || {
        let _status = member.wait();
        let _sent = reaped_tx.send(());
    });
    Deadline::after(DEADLINE)
        .recv(&reaped_rx)
        .expect("the killed member was reaped");

    clock.advance_when_parked(
        Some(after_bound),
        None,
        super::POLL,
        &Deadline::after(DEADLINE),
    );
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("retire returned once the group was empty");
    assert!(!listed(pgid));
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_new_seq_counts_and_a_replay_does_not() {
    let mut last = None;
    assert!(note_seq(&mut last, Some(1)));
    assert!(!note_seq(&mut last, Some(1)), "a replayed seq is not new");
    assert!(!note_seq(&mut last, Some(0)), "an older seq is not new");
    assert!(note_seq(&mut last, Some(2)));
    assert!(
        note_seq(&mut last, None),
        "a line without seq always counts"
    );
}

#[test]
fn a_blocked_watch_still_lets_a_stop_through() {
    let dir = TempDir::new("fiber-delegate-blocked-stop");
    let pidfile = dir.path().join("pid");
    let ready = dir.path().join("ready");
    let block = fifo(dir.path(), "block");
    let shell = format!(
        "echo $$ > '{}'; trap '' TERM; echo ready > '{}'; read _ < '{}'",
        pidfile.display(),
        ready.display(),
        block.display()
    );
    // The watch blocks until the test releases it; the runner must still
    // supervise the child: one watcher runs, and the stop timer fires on
    // the clock without it.
    let (release, gate) = mpsc::channel();
    let rig = rig(vec![WatchReply::Block(gate)]);
    let done = rig.start(&shell, BOUND, 1024);
    let pgid = pid_in_file(&pidfile);
    let watchdog = Watchdog::group(pgid);
    wait_ready(&ready);
    within("the first watch call", Deadline::after(DEADLINE), {
        let script = Arc::clone(&rig.script);
        move || {
            while script.calls().is_empty() {
                thread::yield_now();
            }
        }
    });
    assert_eq!(rig.registry.stop_delegates(), 1);
    // The watch is blocked, so `outstanding` keeps the retry out of the
    // deadline: the runner parks at each poll horizon, then at the stop
    // bound (docs/testing.md, "Waits and timeouts": a test advances a
    // fake clock only after a signal that the code under test is waiting
    // on that clock). Nothing moves the clock before the stop, so the
    // bound is five polls on from the origin.
    let t0 = rig.clock.origin();
    for k in 1..=5 {
        let at = t0 + Duration::from_secs(k);
        assert!(
            rig.clock.await_parked(at, fakes::Deadline::after(DEADLINE)),
            "the runner parks until {at:?}"
        );
        rig.clock
            .advance_when_parked(Some(at), None, super::POLL, &Deadline::after(DEADLINE));
    }
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Cancelled);
    assert_eq!(
        notice
            .completed
            .process
            .as_ref()
            .and_then(|process| process.signal.clone()),
        Some("SIGKILL".to_owned())
    );
    assert!(
        crate::delegate::group::sent_signals()
            .iter()
            .any(|(signalled, signal)| *signalled == pgid
                && *signal == rustix::process::Signal::KILL),
        "SIGKILL went out once the bound passed"
    );
    // Waiting for the runner to return rules out later watches and timer signals.
    Deadline::after(DEADLINE)
        .recv(&done)
        .expect("the runner returns after the stop");
    assert_eq!(
        rig.script.calls().len(),
        1,
        "one watcher runs while it is blocked, never one per wake"
    );
    drop(release);
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn a_blocked_watch_does_not_block_the_cap() {
    let dir = TempDir::new("fiber-delegate-blocked-cap");
    let block = fifo(dir.path(), "block");
    let shell = format!(
        "for i in 1 2 3 4; do echo filler-line >> '{}'; done; read _ < '{}'",
        "EVENTS",
        block.display()
    );
    let (release, gate) = mpsc::channel();
    let rig = rig(vec![WatchReply::Block(gate)]);
    let shell = shell.replace("EVENTS", &rig.events.to_string_lossy());
    // Backstop: kills the group if the test fails before the child exits.
    let watchdog = Watchdog::matching(&block.to_string_lossy());
    rig.start(&shell, BOUND, 32);
    // No line is ever delivered: the backoff wakes alone trip the cap.
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::OutputCap)
    );
    drop(release);
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn park_due_takes_the_earliest_live_bound() {
    let clock = FakeClock::new();
    let t0 = clock.now();
    let poll = t0 + Duration::from_secs(1);
    // A due retry pulls the deadline in.
    assert_eq!(
        park_due(
            poll,
            false,
            false,
            t0 + Duration::from_millis(50),
            None,
            None
        ),
        t0 + Duration::from_millis(50)
    );
    // A future retry leaves the horizon alone.
    assert_eq!(
        park_due(poll, false, false, t0 + Duration::from_secs(2), None, None),
        poll
    );
    // Once exited, a stale retry never pulls the deadline back: that
    // would spin instead of parking.
    assert_eq!(park_due(poll, true, false, t0, None, None), poll);
    // While a watcher runs, its stale retry never pulls it back either.
    assert_eq!(park_due(poll, false, true, t0, None, None), poll);
    // The stop timer clamps everything, exited or not.
    assert_eq!(
        park_due(
            poll,
            true,
            false,
            t0,
            Some(t0 + Duration::from_millis(500)),
            None
        ),
        t0 + Duration::from_millis(500)
    );
    assert_eq!(
        park_due(
            poll,
            false,
            false,
            t0 + Duration::from_millis(50),
            Some(t0 + Duration::from_millis(500)),
            None
        ),
        t0 + Duration::from_millis(50)
    );
}

#[test]
fn kill_due_matches_the_exact_boundaries() {
    let clock = FakeClock::new();
    let kill_at = clock.now() + Duration::from_secs(1);
    let tick = Duration::from_nanos(1);
    let before = kill_at.checked_sub(tick).unwrap();
    let after = kill_at.checked_add(tick).unwrap();

    for (case, kill_at, now, expected) in [
        ("no bound", None, kill_at, false),
        ("one tick before", Some(kill_at), before, false),
        ("at the bound", Some(kill_at), kill_at, true),
        ("one tick after", Some(kill_at), after, true),
    ] {
        assert_eq!(kill_due(kill_at, now), expected, "{case}");
    }
}

#[test]
fn cap_due_checks_once_per_poll_interval() {
    // Border table for the interval comparison: just below the interval
    // no check is due, exactly at it and just above one is. The below
    // row kills a `<=` or inverted mutant that would stat every
    // envelope; the at row kills a `>` mutant that would skip the due
    // check; the above row kills an `==` mutant that would check only at
    // the exact instant.
    let clock = FakeClock::new();
    let last = clock.now();
    assert!(cap_due(last, None, POLL), "the first envelope always stats");
    let tick = Duration::from_nanos(1);
    for (case, now, expected) in [
        (
            "just below the interval",
            last + POLL.checked_sub(tick).unwrap(),
            false,
        ),
        ("at the interval", last + POLL, true),
        ("just above the interval", last + POLL + tick, true),
    ] {
        assert_eq!(cap_due(now, Some(last), POLL), expected, "{case}");
    }
}

#[test]
fn over_len_trips_only_past_the_cap() {
    for (case, len, expected) in [
        ("below the cap", 7, false),
        ("at the cap", 8, false),
        ("past the cap", 9, true),
    ] {
        assert_eq!(over_len(len, 8), expected, "{case}");
    }
}

/// Runs one watch call delivering one envelope per `seq`, against `stat`.
fn run_watch(
    clock: &Arc<FakeClock>,
    shared: &Arc<Shared>,
    last_check: &Arc<Mutex<Option<Instant>>>,
    stat: Stat,
    cap: u64,
    seqs: std::ops::Range<u64>,
) {
    let lines: Vec<Envelope> = seqs.map(|seq| fiber_exited_line(seq, "Line.")).collect();
    let watch: Watch = Arc::new(
        move |_: &SessionId, _: Option<u64>, on_line: &mut dyn FnMut(&Envelope)| {
            for line in &lines {
                on_line(line);
            }
            Ok(Watched::Closed)
        },
    );
    let waker: Arc<dyn contract::clock::Wake> = Arc::new(Parker::new());
    Watcher {
        watch,
        session_id: SessionId("s_child".into()),
        after: None,
        fold: Arc::new(Mutex::new(Fold::default())),
        shared: Arc::clone(shared),
        cap,
        clock: Arc::clone(clock) as Arc<dyn contract::clock::Clock>,
        last_check: Arc::clone(last_check),
        stat,
        output_path: std::path::PathBuf::from("missing.log"),
        wake: waker,
        on_usage: None,
    }
    .run();
}

#[test]
fn the_watch_stats_the_log_once_per_poll_interval() {
    // Five envelopes inside one interval cost one stat; with the log
    // past the cap but the clock unmoved, no second stat sees it; past
    // the interval the next envelope stats again and the crossing halts
    // the job. The watch returns at once, so nothing parks on the clock
    // and no wait or deadline is needed.
    let clock = FakeClock::new();
    let stats = Arc::new(Mutex::new(0usize));
    let size = Arc::new(Mutex::new(0u64));
    let stat: Stat = {
        let stats = Arc::clone(&stats);
        let size = Arc::clone(&size);
        Arc::new(move || {
            *stats.lock().unwrap() += 1;
            *size.lock().unwrap()
        })
    };
    let as_clock: Arc<dyn contract::clock::Clock> = Arc::clone(&clock) as Arc<_>;
    let shared = Shared::new(as_clock, BOUND);
    let last_check: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let cap = 8u64;

    run_watch(&clock, &shared, &last_check, Arc::clone(&stat), cap, 1..6);
    assert_eq!(*stats.lock().unwrap(), 1, "one interval costs one stat");
    assert_eq!(shared.reason(), None);

    *size.lock().unwrap() = cap + 1;
    run_watch(&clock, &shared, &last_check, Arc::clone(&stat), cap, 6..7);
    assert_eq!(
        *stats.lock().unwrap(),
        1,
        "inside the interval nothing stats again"
    );
    assert_eq!(shared.reason(), None, "an unseen crossing halts nothing");

    // No code under test waits on the clock here; it moves the interval the stat check reads (ruling 6179406086).
    clock.advance(POLL);
    run_watch(&clock, &shared, &last_check, Arc::clone(&stat), cap, 7..8);
    assert_eq!(*stats.lock().unwrap(), 2, "past the interval it stats");
    assert_eq!(shared.reason(), Some(Termination::OutputCap));
}

#[test]
fn the_watch_boundary_skips_an_earlier_run_exit() {
    // The child prints the same line the watch delivers for this run:
    // whether the reap or the watch wins the race, the fold sees the
    // live run's `fiber_exited`, never the replayed earlier one. The
    // reap may win before any watch runs, so the list may be empty.
    let line = json_line(9, "New.");
    let rig = rig(vec![WatchReply::Exited(vec![
        fiber_exited_line(5, "Old."),
        fiber_exited_line(9, "New."),
    ])]);
    let watchdog = Watchdog::matching("fiber-delegate-boundary");
    rig.start_after(
        &format!(": fiber-delegate-boundary; printf '%s\\n' '{line}'; exit 0"),
        BOUND,
        1024,
        Some(5),
    );
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("New.".into())
    );
    assert!(
        rig.script.afters().iter().all(|after| *after == Some(5)),
        "every watch call passes the boundary (none when the reap wins): {:?}",
        rig.script.afters()
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn no_boundary_takes_the_first_replayed_exit_as_today() {
    // The child prints the same line the watch delivers first: whether
    // the reap or the watch wins the race, the fold sees that exit, as
    // it did before any boundary. The reap may win before any watch
    // runs, so the list may be empty.
    let line = json_line(5, "Old.");
    let rig = rig(vec![WatchReply::Exited(vec![
        fiber_exited_line(5, "Old."),
        fiber_exited_line(9, "New."),
    ])]);
    let watchdog = Watchdog::matching("fiber-delegate-no-boundary");
    rig.start(
        &format!(": fiber-delegate-no-boundary; printf '%s\\n' '{line}'; exit 0"),
        BOUND,
        1024,
    );
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("Old.".into())
    );
    assert!(
        rig.script.afters().iter().all(|after| after.is_none()),
        "every watch call passes no boundary (none when the reap wins): {:?}",
        rig.script.afters()
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn the_fold_starts_from_the_run_boundary() {
    // The fold dedupes from the run's boundary: a replayed earlier run's
    // `fiber_exited` at the boundary never stands in for this run's, even
    // when the watch delivers it. The fake script filters at the boundary
    // as the real watch does, so this test drives the runner with a watch
    // that replays without filtering: the fold is the backstop. The child
    // blocks until the watch folded the stale line, so the stale line is always first; the
    // fresh line on stdout ends the run. Without the boundary the fold
    // takes the stale line and the run reports it instead.
    let rig = rig(vec![]);
    let stale = fiber_exited_line(5, "Old.");
    let fresh = json_line(9, "New.");
    let (called_tx, called_rx) = mpsc::channel();
    let seen: Arc<Mutex<Vec<Option<u64>>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_after = Arc::clone(&seen);
    let watch: Watch = Arc::new(
        move |_: &SessionId, after: Option<u64>, on_line: &mut dyn FnMut(&Envelope)| {
            seen_after.lock().unwrap().push(after);
            on_line(&stale);
            let _sent = called_tx.send(());
            Ok(Watched::Exited)
        },
    );
    let gate = TempDir::new("fiber-delegate-fold-boundary");
    let block = fifo(gate.path(), "block");
    let shell = format!(
        ": fiber-delegate-fold-boundary; read _ < '{}'; printf '%s\\n' '{fresh}'; exit 0",
        block.display()
    );
    let events = rig.events.clone();
    let launch: Launch = Arc::new(move |_: &Launched| {
        let mut command = Command::new("sh");
        command.args(["-c", shell.as_str()]);
        command
    });
    let (runner, stop) = Runner::new(
        rig.job.clone(),
        SessionId("s_child".into()),
        events.clone(),
        Arc::clone(&rig.clock) as Arc<dyn contract::clock::Clock>,
        BOUND,
        1024,
        watch,
        Some(5),
        None,
        None,
    );
    let launched = Launched {
        depth: 1,
        session_id: SessionId("s_child".into()),
        job_id: rig.job.clone(),
        parent: SessionId("s_parent".into()),
        prompt: "hi".into(),
        workspace: rig.events.parent().unwrap().to_path_buf(),
        start: Start::Fresh(Fresh::Spawn {
            model: "fake/m".into(),
            credential: None,
        }),
    };
    let (child, listing) = runner.spawn(&launch, &launched).unwrap();
    let (_started, finish) = rig.registry.open_started(
        rig.job.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        events.to_string_lossy().into_owned(),
        stop,
        crate::support::delegate_started(&rig.job.0),
    );
    let watchdog = Watchdog::matching("fiber-delegate-fold-boundary");
    thread::spawn(move || {
        runner.drive(child, Some(listing), finish);
    });
    Deadline::after(DEADLINE)
        .recv(&called_rx)
        .expect("the watch ran before the release");
    release_fifo(&block, b"go\n");
    let notice = reported(&rig);
    assert_eq!(notice.completed.status, Outcome::Completed);
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some("New.".into())
    );
    let afters = seen.lock().unwrap().clone();
    assert!(
        !afters.is_empty() && afters.iter().all(|after| *after == Some(5)),
        "every watch call passes the boundary: {afters:?}"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn minted_command_ids_carry_their_prefix_and_differ() {
    let first = super::mint_command_id();
    let second = super::mint_command_id();
    for id in [&first, &second] {
        assert!(
            id.starts_with("c_"),
            "a command id carries its prefix: {id}"
        );
    }
    assert_ne!(first, second, "two mints differ");
}

/// A `usage_recorded` envelope as a delegate writes it: `seq` orders the
/// stream, the payload is the call's [`UsageRecorded`].
fn usage_envelope(seq: u64, generation: &str, cost: Option<f64>) -> Envelope {
    use contract::events::UsageRecorded;
    use contract::shapes::Tokens;
    use contract::{GenerationId, SessionId as Sid};
    let _ = Sid;
    let recorded = UsageRecorded {
        generation_id: GenerationId(generation.into()),
        model: "fake/m".into(),
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 5,
        },
        input_bytes: 100,
        input_media: None,
        web_searches: None,
        cost,
        subscription: None,
        extension: None,
        origin_session_id: None,
        reviewer: None,
    };
    envelope(
        seq,
        "usage_recorded",
        serde_json::to_value(recorded).unwrap(),
    )
}

#[test]
fn the_stream_forwards_live_usage_past_the_run_boundary() {
    // The socket streams two live usage lines past `after`: the stream
    // claims each under the fold lock and forwards both in order. A
    // boundary check that drops past-`after` lines would forward none;
    // a dedup that forgets the first claim would forward the first
    // twice and drop the second. The shell prints the run's
    // `fiber_exited` to stdout: the drain folds it before the report,
    // so the notice is `Completed` whether or not the watcher beat the
    // reap (see #3005).
    let rig = rig(vec![WatchReply::Exited(vec![
        usage_envelope(2, "g_2", Some(0.1)),
        usage_envelope(3, "g_3", Some(0.2)),
        fiber_exited_line(4, "Done."),
    ])]);
    let exit = serde_json::to_string(&fiber_exited_line(4, "Done.")).unwrap();
    let shell = format!("printf '%s\\n' '{exit}'");
    let waited = rig
        .start_timeout(
            &shell,
            BOUND,
            1 << 20,
            Some(1),
            None,
            Some(rig.registry.usage_forward()),
        )
        .0;
    let _waited = waited;
    // The watcher's forwards and the runner's reap race (see #3005):
    // the take holds the Job and both lines whichever lands first, so
    // no schedule changes which values land. The bound is liveness
    // only, the plan's MUST_SUCCEED_WITHIN.
    let (notice, usages) = take_report_with_usages(&rig, 2);
    let generations: Vec<&str> = usages
        .iter()
        .map(|(_, generation)| generation.as_str())
        .collect();
    assert_eq!(generations, ["g_2", "g_3"]);
    assert_eq!(notice.completed.status, Outcome::Completed);
}

/// Takes the runner's Job notice and `want` streamed usages in either
/// order (see #3005): stopping at the first Job would drop the lines
/// when the reap beats the watcher, and stopping at the lines would
/// drop the Job when the watcher wins. One scoped clock driver serves
/// the take, as [`reported_with_usage_before`]'s does; the bound is
/// liveness only, the plan's MUST_SUCCEED_WITHIN. The failure names
/// which half never arrived.
#[track_caller]
fn take_report_with_usages(
    rig: &Rig,
    want: usize,
) -> (contract::inbox::JobNotice, Vec<(SessionId, String)>) {
    let stop = Arc::new(AtomicBool::new(false));
    let clock = Arc::clone(&rig.clock);
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    thread::scope(|scope| {
        scope.spawn(|| drive_runner_clock(&stop, &clock, &deadline, None));
        let mut usages = Vec::new();
        let mut notice = None;
        while notice.is_none() || usages.len() < want {
            match deadline.recv(&rig.inbox) {
                Ok(Delivery::Job(got)) => {
                    if notice.is_none() {
                        notice = Some(got);
                    }
                }
                Ok(Delivery::DelegateUsage { session, line }) => {
                    usages.push((session, line.generation_id.0.clone()));
                }
                Ok(other) => {
                    stop.store(true, Ordering::Relaxed);
                    panic!("expected the report or the streamed usage, got {other:?}");
                }
                Err(_) => {
                    stop.store(true, Ordering::Relaxed);
                    panic!(
                        "the report and the streamed usage never arrived; job={} usages={want} got={}",
                        notice.is_some(),
                        usages.len(),
                    );
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        (notice.unwrap(), usages)
    })
}

/// [`reported`], collecting the `DelegateUsage` deliveries the watcher's
/// tap forwards ahead of the runner's report: the shared core stashes
/// each while the take loops past them, until the `Job` notice arrives.
#[track_caller]
fn reported_with_usage(rig: &Rig) -> (contract::inbox::JobNotice, Vec<(SessionId, String)>) {
    reported_with_usage_before(rig, None)
}

/// The parked forward's gate, shared with the watcher: the test owns
/// the sender half and releases the park by dropping it, so a failed
/// test unwinds the sender first and never leaves the watcher parked
/// (as the `Block` watch reply). Parking forces the order a loaded
/// machine produces by accident (see #3051): the report lands with
/// nothing stashed, and the drain collects every line after the
/// release, instead of racing the watcher's scheduling.
struct Park {
    rx: Mutex<mpsc::Receiver<()>>,
}

impl Park {
    /// A closed gate: the sender half that releases it, and the shared
    /// half the forward parks on.
    fn closed() -> (mpsc::Sender<()>, Arc<Self>) {
        let (tx, rx) = mpsc::channel();
        (tx, Arc::new(Self { rx: Mutex::new(rx) }))
    }

    /// Waits for the release under the plan's bound: expiry names the
    /// missing release instead of hanging the watcher, and a dropped
    /// sender (a failed test's unwind) releases at once.
    fn wait(&self) {
        match Deadline::after(MUST_SUCCEED_WITHIN).recv(&self.rx.lock().unwrap()) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("the report never released the parked forward")
            }
        }
    }
}

/// The registry's forward behind a release gate: every watcher line
/// parks until the test drops the sender half, then forwards as usual.
/// The runner's reap and report never wait on the watcher, so parking
/// cannot deadlock the report the release waits for.
fn parked_forward(rig: &Rig, park: &Arc<Park>) -> super::UsageForward {
    let forward = rig.registry.usage_forward();
    let park = Arc::clone(park);
    Arc::new(
        move |session: SessionId, line: contract::events::UsageRecorded| {
            park.wait();
            forward(session, line);
        },
    )
}

/// Collects streamed `DelegateUsage` deliveries until `want` have
/// arrived, after the run already reported: the watcher's forwards
/// always follow on the inbox (the tap runs before each offer), so a
/// Job that precedes them (see #3005) only delays this drain. The
/// bound is the plan's MUST_SUCCEED_WITHIN, the same bound the parked
/// forward's release waits on: under saturation the watcher thread can
/// go unscheduled past the old 3s report deadline (see #3001).
fn drain_streamed_usages(
    rig: &Rig,
    mut usages: Vec<(SessionId, String)>,
    want: usize,
) -> Vec<(SessionId, String)> {
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    while usages.len() < want {
        match deadline.recv(&rig.inbox) {
            Ok(Delivery::DelegateUsage { session, line }) => {
                usages.push((session, line.generation_id.0.clone()));
            }
            Ok(other) => panic!("expected the streamed usage, got {other:?}"),
            Err(_) => panic!("the streamed usage never arrived"),
        }
    }
    usages
}

#[test]
fn the_watcher_tap_forwards_well_formed_usage_and_drops_malformed() {
    let rig = rig(vec![WatchReply::Exited(vec![
        usage_envelope(1, "g_1", Some(0.1)),
        envelope(2, "usage_recorded", serde_json::json!({})),
        fiber_exited_line(3, "Done."),
    ])]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let tapped = Arc::clone(&seen);
    rig.registry.install_usage_tap(Some(Arc::new(move |line| {
        tapped.lock().unwrap().push(line.generation_id.0.clone());
    })));
    // The shell prints the run's `fiber_exited` to stdout: the drain
    // folds it before the report, so the notice is `Completed` whether
    // or not the watcher beat the reap (see #3005).
    let exit = serde_json::to_string(&fiber_exited_line(3, "Done.")).unwrap();
    let shell = format!("printf '%s\\n' '{exit}'");
    // The forward parks until the report lands: a loaded machine
    // schedules the watcher after the drain otherwise (see #3051).
    let (release, park) = Park::closed();
    let waited = rig
        .start_timeout(
            &shell,
            BOUND,
            1 << 20,
            None,
            None,
            Some(parked_forward(&rig, &park)),
        )
        .0;
    let _waited = waited;
    let (notice, usages) = reported_with_usage(&rig);
    // Nothing is stashed: every line is still parked behind the
    // release, which proves the hold before the drain releases it.
    assert!(usages.is_empty(), "the parked forward holds every line");
    drop(release);
    // The well-formed line reaches the tap with the delegate's session id;
    // the malformed one never does, and `fiber_exited` still ends watch.
    // The Job can precede the watcher's forward, so the tap is read
    // only after the streamed line lands on the inbox.
    let usages = drain_streamed_usages(&rig, usages, 1);
    assert_eq!(seen.lock().unwrap().as_slice(), ["g_1"]);
    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0].0, SessionId("s_child".into()));
    assert_eq!(usages[0].1, "g_1");
    assert_eq!(notice.completed.status, Outcome::Completed);
}

/// Runs one watch call against a log file holding `file_seqs` usage
/// lines while the socket yields `streamed` then closes, collecting the
/// forwarded generations in order: what the backfill replays, what the
/// socket streams, and what neither sends twice.
fn run_backfill(
    file_seqs: &[u64],
    streamed: Vec<Envelope>,
    after: Option<u64>,
) -> Vec<(SessionId, String)> {
    let dir = TempDir::new("fiber-backfill");
    let path = dir.path().join("events.jsonl");
    let mut text = String::new();
    for seq in file_seqs {
        text.push_str(
            &serde_json::to_string(&usage_envelope(*seq, &format!("g_{seq}"), Some(0.1))).unwrap(),
        );
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let tapped = Arc::clone(&seen);
    let on_usage: super::UsageForward = Arc::new(
        move |session: SessionId, line: contract::events::UsageRecorded| {
            tapped
                .lock()
                .unwrap()
                .push((session, line.generation_id.0.clone()));
        },
    );
    let watch: Watch = Arc::new(
        move |_: &SessionId, _: Option<u64>, on_line: &mut dyn FnMut(&Envelope)| {
            for line in &streamed {
                on_line(line);
            }
            Ok(Watched::Closed)
        },
    );
    let clock = FakeClock::new();
    let waker: Arc<dyn contract::clock::Wake> = Arc::new(Parker::new());
    Watcher {
        watch,
        session_id: SessionId("s_child".into()),
        after,
        fold: Arc::new(Mutex::new(Fold::default())),
        shared: Shared::new(Arc::clone(&clock) as Arc<dyn contract::clock::Clock>, BOUND),
        cap: 1 << 20,
        clock: Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        last_check: Arc::new(Mutex::new(None)),
        stat: Arc::new(|| 0),
        output_path: path,
        wake: waker,
        on_usage: Some(on_usage),
    }
    .run();
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .all(|(session, _)| *session == SessionId("s_child".into())),
        "every forward names the delegate's session: {seen:?}"
    );
    seen
}

#[test]
fn the_backfill_forwards_file_usage_the_socket_missed() {
    // The socket closes without yielding: a delegate that exits before
    // the first subscribe connects still forwards its file's usage, in
    // `seq` order.
    let seen = run_backfill(&[1, 2], Vec::new(), None);
    let generations: Vec<&str> = seen
        .iter()
        .map(|(_, generation)| generation.as_str())
        .collect();
    assert_eq!(generations, ["g_1", "g_2"]);
}

#[test]
fn the_backfill_skips_what_the_socket_streamed() {
    // The socket streams `g_1` live; the file holds `g_1` and `g_2`: the
    // backfill sends only `g_2`, so neither path forwards a `seq` twice.
    let seen = run_backfill(&[1, 2], vec![usage_envelope(1, "g_1", Some(0.1))], None);
    let generations: Vec<&str> = seen
        .iter()
        .map(|(_, generation)| generation.as_str())
        .collect();
    assert_eq!(generations, ["g_1", "g_2"]);
}

#[test]
fn the_backfill_skips_lines_at_or_before_after() {
    // The run's boundary is `after`: a line at or before it belongs to
    // an earlier run, so the backfill forwards only past it.
    let seen = run_backfill(&[1, 2], Vec::new(), Some(1));
    let generations: Vec<&str> = seen
        .iter()
        .map(|(_, generation)| generation.as_str())
        .collect();
    assert_eq!(generations, ["g_2"]);
}

#[test]
fn the_reap_replays_file_usage_the_socket_never_saw() {
    // The socket closes without yielding: the delegate writes its usage
    // straight to its log file and exits. The reap's file replay still
    // forwards the line once, whichever of the watch-end and reap-end
    // replays runs first.
    let rig = rig(vec![WatchReply::Closed(Vec::new())]);
    let line = serde_json::to_string(&usage_envelope(9, "g_9", Some(0.5))).unwrap();
    let shell = format!(
        "printf '%s\\n' '{line}' >> '{}'; exit 0",
        rig.events.display()
    );
    let waited = rig
        .start_timeout(
            &shell,
            BOUND,
            1 << 20,
            None,
            None,
            Some(rig.registry.usage_forward()),
        )
        .0;
    let _waited = waited;
    let (notice, usages) = reported_with_usage(&rig);
    assert_eq!(usages.len(), 1, "the file line forwards once: {usages:?}");
    assert_eq!(usages[0].0, SessionId("s_child".into()));
    assert_eq!(usages[0].1, "g_9");
    // The bare shell reports no `fiber_exited`, so the job ends without
    // a result; the forward is what this test proves.
    let _ = notice;
}

/// Runs two file replays sharing one fold against a pre-written two-line
/// usage file, forcing the loser's claim inside the winner's forward:
/// the winner's first forward signals `entered`, then parks at the
/// barrier until the test has run the loser to completion and released
/// it. Later forwards pass through, so the barrier sees exactly two
/// parties on old and new code alike. The `entered` receive is the
/// signal the winner is waiting (docs/testing.md, "Waits and timeouts");
/// the barrier release is the loser's completion. Returns the forwarded
/// generations in forward order.
fn raced_replays() -> Vec<String> {
    let dir = TempDir::new("fiber-replay-race");
    let path = dir.path().join("events.jsonl");
    let mut text = String::new();
    for seq in [9, 10] {
        text.push_str(
            &serde_json::to_string(&usage_envelope(seq, &format!("g_{seq}"), Some(0.5))).unwrap(),
        );
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
    let fold = Arc::new(Mutex::new(Fold::default()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (entered_tx, entered_rx) = mpsc::channel::<String>();
    let release = Arc::new(Barrier::new(2));
    let parked = Arc::new(AtomicBool::new(false));
    let session = SessionId("s_child".into());
    let on_usage: super::UsageForward = {
        let seen = Arc::clone(&seen);
        let release = Arc::clone(&release);
        let parked = Arc::clone(&parked);
        Arc::new(move |_: SessionId, line: contract::events::UsageRecorded| {
            seen.lock().unwrap().push(line.generation_id.0.clone());
            let _sent = entered_tx.send(line.generation_id.0.clone());
            if !parked.swap(true, Ordering::SeqCst) {
                release.wait();
            }
        })
    };
    let first = {
        let path = path.clone();
        let fold = Arc::clone(&fold);
        let session = session.clone();
        let on_usage = on_usage.clone();
        thread::spawn(move || {
            Watcher::replay_usage(&path, None, &fold, &session, &Some(on_usage));
        })
    };
    // The winner is parked inside its forward; the loser now runs to
    // completion on this thread, so the order is forced, never raced.
    let entered = Deadline::after(DEADLINE).recv(&entered_rx);
    assert!(entered.is_ok(), "the first replay forwards");
    Watcher::replay_usage(&path, None, &fold, &session, &Some(Arc::clone(&on_usage)));
    release.wait();
    first.join().unwrap();
    let forwarded = seen.lock().unwrap().clone();
    assert_eq!(
        forwarded,
        ["g_9", "g_10"],
        "the file lines forward once: {forwarded:?}"
    );
    // A third replay past the race still forwards nothing: the claim
    // holds, so neither a smaller `seq` nor a repeat slips through.
    Watcher::replay_usage(&path, None, &fold, &session, &Some(Arc::clone(&on_usage)));
    seen.lock().unwrap().clone()
}

#[test]
fn the_watch_replay_wins_when_it_runs_first() {
    // The spawned thread stands for the watch-end backfill, this thread
    // for the reap-end replay: the watch claims the line before the reap
    // runs, the reap finds it sent and forwards nothing, so the line
    // forwards exactly once. Both orders land on the one shared claim.
    // Flipping the claim back to check-then-act lets the reap claim
    // inside the watch's forward and forwards twice.
    let seen = raced_replays();
    assert_eq!(
        seen,
        ["g_9", "g_10"],
        "the file lines forward once: {seen:?}"
    );
}

/// The running spend the tap drives: a budget, and a stop-all counter
/// standing in for the loop's delegate stop. Per-generation delta
/// arithmetic is production logic owned loop-side (see the tripwire
/// `pending_tests`); this fake only accumulates, never folds.
struct TripTap {
    sum: Mutex<f64>,
    budget: f64,
    tripped: AtomicBool,
    stops: std::sync::atomic::AtomicUsize,
}

impl TripTap {
    fn tap(&self, line: &contract::events::UsageRecorded) {
        let sum = {
            let mut sum = self.sum.lock().unwrap();
            *sum += line.cost.unwrap_or(0.0);
            *sum
        };
        if sum >= self.budget && !self.tripped.swap(true, Ordering::SeqCst) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn sum(&self) -> f64 {
        *self.sum.lock().unwrap()
    }

    fn stops(&self) -> usize {
        self.stops.load(Ordering::SeqCst)
    }
}

#[test]
fn the_trip_calls_stop_once_on_crossing_and_never_before() {
    // The socket closes without yielding and the log file below holds
    // three usage lines crossing the budget: the crossing runs on the
    // synchronous file-replay path, never on the unjoined watcher
    // thread, so the sum, the single stop and the three forwards hold
    // whichever replay wins the claim (see #3005). The reap's replay
    // runs before the report, so all three lines land first. Streamed
    // delivery stays covered by the tap tests.
    let rig = rig(vec![WatchReply::Closed(Vec::new())]);
    let mut text = String::new();
    for (seq, generation, cost) in [(1, "g_1", 0.1), (2, "g_2", 0.15), (3, "g_3", 0.1)] {
        text.push_str(
            &serde_json::to_string(&usage_envelope(seq, generation, Some(cost))).unwrap(),
        );
        text.push('\n');
    }
    std::fs::write(&rig.events, text).unwrap();
    let trip = Arc::new(TripTap {
        sum: Mutex::new(0.0),
        budget: 0.2,
        tripped: AtomicBool::new(false),
        stops: std::sync::atomic::AtomicUsize::new(0),
    });
    let tapped = Arc::clone(&trip);
    rig.registry.install_usage_tap(Some(Arc::new(move |line| {
        tapped.tap(&line);
    })));
    let waited = rig
        .start_timeout(
            "exit 0",
            BOUND,
            1 << 20,
            None,
            None,
            Some(rig.registry.usage_forward()),
        )
        .0;
    let _waited = waited;
    let (notice, usages) = reported_with_usage(&rig);
    // Below the budget nothing trips; the crossing trips once; the line
    // after never trips again.
    assert_eq!(trip.sum(), 0.35);
    assert_eq!(trip.stops(), 1);
    assert_eq!(usages.len(), 3);
    // The bare shell and the closed socket report no `fiber_exited`,
    // so the job ends without a result; the trip is what this test
    // proves.
    assert_eq!(notice.completed.status, Outcome::Failed);
}

#[test]
fn a_correction_applies_only_its_delta() {
    let rig = rig(vec![WatchReply::Exited(vec![
        usage_envelope(1, "g_1", Some(0.1)),
        usage_envelope(2, "g_1", Some(0.18)),
        fiber_exited_line(3, "Done."),
    ])]);
    let trip = Arc::new(TripTap {
        sum: Mutex::new(0.0),
        budget: 10.0,
        tripped: AtomicBool::new(false),
        stops: std::sync::atomic::AtomicUsize::new(0),
    });
    let tapped = Arc::clone(&trip);
    rig.registry.install_usage_tap(Some(Arc::new(move |line| {
        tapped.tap(&line);
    })));
    // The shell prints the run's `fiber_exited` to stdout: the drain
    // folds it before the report, so the notice is `Completed` whether
    // or not the watcher beat the reap (see #3005).
    let exit = serde_json::to_string(&fiber_exited_line(3, "Done.")).unwrap();
    let shell = format!("printf '%s\\n' '{exit}'");
    let waited = rig
        .start_timeout(
            &shell,
            BOUND,
            1 << 20,
            None,
            None,
            Some(rig.registry.usage_forward()),
        )
        .0;
    let _waited = waited;
    // The watcher's forwards and the runner's reap race (see #3005):
    // the take holds the Job and both lines whichever lands first, so
    // no schedule changes which values land, and the tap is read only
    // after the take holds both streamed lines. The bound is liveness
    // only, the plan's MUST_SUCCEED_WITHIN.
    let (notice, usages) = take_report_with_usages(&rig, 2);
    // Both arrivals of the corrected generation stream exactly once,
    // and neither trips the budget. The delta arithmetic itself is
    // production logic proved loop-side (the tripwire `pending_tests`),
    // so no per-generation sum is asserted here.
    assert_eq!(trip.stops(), 0);
    assert_eq!(usages.len(), 2);
    assert_eq!(notice.completed.status, Outcome::Completed);
}

#[test]
fn sibling_arrivals_racing_a_publication_lose_no_delta_and_trip_once() {
    let rig = rig(vec![WatchReply::Exited(vec![fiber_exited_line(
        1, "Done.",
    )])]);
    let trip = Arc::new(TripTap {
        sum: Mutex::new(0.0),
        budget: 0.5,
        tripped: AtomicBool::new(false),
        stops: std::sync::atomic::AtomicUsize::new(0),
    });
    rig.registry.install_usage_tap(Some(Arc::new({
        let trip = Arc::clone(&trip);
        move |line| trip.tap(&line)
    })));
    let forward = rig.registry.usage_forward();
    // Sibling watcher threads forward distinct generations while the
    // publication reads beside them: every delta lands, and the crossing
    // trips exactly once.
    std::thread::scope(|scope| {
        for thread in 0..8 {
            let forward = Arc::clone(&forward);
            scope.spawn(move || {
                for n in 0..4 {
                    let generation = format!("g_{thread}_{n}");
                    forward(
                        SessionId("s_child".into()),
                        contract::events::UsageRecorded {
                            generation_id: contract::GenerationId(generation),
                            model: "fake/m".into(),
                            tokens: contract::shapes::Tokens {
                                input: 10,
                                cache_read: 0,
                                cache_write: std::collections::BTreeMap::new(),
                                output: 5,
                            },
                            input_bytes: 100,
                            input_media: None,
                            web_searches: None,
                            cost: Some(0.05),
                            subscription: None,
                            extension: None,
                            origin_session_id: None,
                            reviewer: None,
                        },
                    );
                }
            });
        }
    });
    assert!((trip.sum() - 1.6).abs() < 1e-9);
    assert_eq!(trip.stops(), 1);
}
