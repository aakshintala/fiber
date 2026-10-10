//! Running a Fiber delegate: spawning the child session, draining its
//! stdout, watching its socket to its end, and folding how it ended
//! (`docs/delegates.md`, "Lifetime"). The runner owns no model call: it
//! returns nothing to the tool, which answers as soon as the spawn and the
//! record are done.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::events::FiberExited;
use contract::jobs::Stop;
use contract::{Envelope, JobId, SessionId};
use rustix::process::Signal;

use super::group;
use super::outcome::{Termination, outcome};
use crate::registry::Finish;
use support::group::Listing;

/// Resolves a `fiber:` model reference to the `provider/model[:level]` the
/// child is started with, or lists the valid references for the refusal.
pub type Resolve = Arc<dyn Fn(&str) -> Result<String, Vec<String>> + Send + Sync>;

/// Builds the child's command from its launch. The runner sets the stdio,
/// the process group and the lifeline around it.
pub type Launch = Arc<dyn Fn(&Launched) -> Command + Send + Sync>;

/// Watches the delegate's socket, calling `on_line` for each envelope
/// until `fiber_exited` or EOF: the jobs-local mirror of `doors::watch`,
/// which `main` wires up in part 4.
pub type Watch =
    Arc<dyn Fn(&SessionId, &mut dyn FnMut(&Envelope)) -> io::Result<Watched> + Send + Sync>;

/// What a watch returned: the jobs-local mirror of `doors::Watched`.
/// `Closed` carries no `last_seq`: the runner dedupes by `seq` itself,
/// so nothing reads it.
#[derive(Debug)]
pub enum Watched {
    /// A `fiber_exited` line arrived: the session wrote its last line.
    Exited,
    /// The connection closed first.
    Closed,
}

/// What the launcher receives: everything the child's argv needs.
pub struct Launched {
    /// The delegate's session.
    pub session_id: SessionId,
    /// The delegate's job in its parent: its id for life.
    pub job_id: JobId,
    /// The parent session.
    pub parent: SessionId,
    /// `provider/model[:level]`, resolved.
    pub model: String,
    /// The prompt, on argv.
    pub prompt: String,
    /// The parent's workspace, which the delegate shares.
    pub workspace: PathBuf,
}

/// The first retry waits this long after a refused or dropped watch.
const START_BACKOFF: Duration = Duration::from_millis(50);

/// No retry waits longer than this, however often the watch fails.
const MAX_BACKOFF: Duration = Duration::from_secs(1);

/// How far ahead the runner parks at most: exit, cap and timer checks all
/// run on a wake at least this often.
const POLL: Duration = Duration::from_secs(1);

/// Test-only pause between computing the drain deadline and parking on it:
/// the test installs a one-shot hook, which the drain wait takes and runs
/// once. Empty means no effect and production never sets it.
#[cfg(test)]
static AFTER_DRAIN_DEADLINE: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

/// Why the runner itself ended the delegate, and when a stop's SIGKILL is
/// due. The stop closure and the runner thread share it; the first reason
/// set wins.
pub(crate) struct Shared {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
    bound: Duration,
}

struct State {
    reason: Option<Termination>,
    pgid: Option<u32>,
    kill_at: Option<Instant>,
}

impl Shared {
    fn new(clock: Arc<dyn Clock>, bound: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                reason: None,
                pgid: None,
                kill_at: None,
            }),
            clock,
            bound,
        })
    }

    fn set_pgid(&self, pgid: u32) {
        lock(&self.state).pgid = Some(pgid);
    }

    /// Ends the delegate as a stop does: the reason, set once, then
    /// SIGTERM to its group. Returns at once; the runner thread owns the
    /// wait to the bound and the SIGKILL after it.
    pub(crate) fn stop(&self) {
        self.halt(Termination::Stopped);
    }

    /// Sets the reason the runner itself ended the delegate, once, and
    /// sends SIGTERM to its group. Past the first, the reason stands.
    fn halt(&self, reason: Termination) {
        let pgid = {
            let mut state = lock(&self.state);
            if state.reason.is_some() {
                return;
            }
            state.reason = Some(reason);
            state.kill_at = Some(later(self.clock.as_ref(), self.bound));
            state.pgid
        };
        if let Some(pgid) = pgid {
            group::signal(pgid, Signal::TERM);
        }
    }

    fn reason(&self) -> Option<Termination> {
        lock(&self.state).reason
    }

    fn kill_at(&self) -> Option<Instant> {
        lock(&self.state).kill_at
    }
}

fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `bound` past now on the clock. The bound is seconds, so the addition
/// cannot overflow; a miss would only end the wait at once.
fn later(clock: &dyn Clock, bound: Duration) -> Instant {
    clock
        .now()
        .checked_add(bound)
        .unwrap_or_else(|| clock.now())
}

/// Whether the stop's SIGKILL bound has arrived.
fn kill_due(kill_at: Option<Instant>, now: Instant) -> bool {
    kill_at.is_some_and(|kill_at| now >= kill_at)
}

/// What wakes the runner's parks: bumped on every clock move, as the
/// registry's `seq` is, so a wake that lands before the wait is still
/// visible.
struct Parker {
    seq: Mutex<u64>,
    cv: Condvar,
}

impl Wake for Parker {
    fn wake(&self) {
        let mut seq = self.seq.lock().unwrap_or_else(PoisonError::into_inner);
        *seq = seq.wrapping_add(1);
        self.cv.notify_all();
    }
}

impl Parker {
    /// The current generation, for the wait's change check. The runner
    /// snapshots it before reading the fold, the clock and the child,
    /// and parks only while it still equals the snapshot, so a watcher
    /// poke or clock move in between is never slept through.
    fn generation(&self) -> u64 {
        *self.seq.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What the watch feeds the fold, shared with the watcher thread: the
/// last new `seq`, the socket's `fiber_exited` when it received one, and
/// the one-shot result of the running watch.
#[derive(Default)]
struct Fold {
    last_seq: Option<u64>,
    socket: Option<FiberExited>,
    /// A watcher thread is inside the blocking socket read.
    outstanding: bool,
    /// The running watch's result, for the runner to take once.
    result: Option<WatchResult>,
}

/// What one watch call returned, for the runner to take once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchResult {
    /// A `fiber_exited` line arrived: the runner watches no more.
    Exited,
    /// The connection closed or refused: the runner backs off.
    Retry,
}

/// Whether `seq` is a line the watch delivers: a `seq` past the last one
/// seen, or a line without one. A reconnect replays what the fold already
/// saw; those lines are not delivered twice.
pub(crate) fn note_seq(last: &mut Option<u64>, seq: Option<u64>) -> bool {
    match seq {
        Some(seq) => {
            if last.is_some_and(|last| seq <= last) {
                return false;
            }
            *last = Some(seq);
            true
        }
        None => true,
    }
}

/// The runner: the delegate's ids, where its log grows, and what drives
/// it. Built before the spawn; driven on its own thread after the record.
pub(crate) struct Runner {
    job_id: JobId,
    session_id: SessionId,
    output_path: PathBuf,
    clock: Arc<dyn Clock>,
    bound: Duration,
    cap: u64,
    watch: Watch,
    shared: Arc<Shared>,
    park: Arc<Parker>,
}

impl Runner {
    /// The runner and its stop. The stop goes to the record; the spawn
    /// sets the group the stop signals.
    pub(crate) fn new(
        job_id: JobId,
        session_id: SessionId,
        output_path: PathBuf,
        clock: Arc<dyn Clock>,
        bound: Duration,
        cap: u64,
        watch: Watch,
    ) -> (Self, Stop) {
        let shared = Shared::new(Arc::clone(&clock), bound);
        let stopping = Arc::clone(&shared);
        let stop = Stop(Box::new(move || stopping.stop()));
        let park = Arc::new(Parker {
            seq: Mutex::new(0),
            cv: Condvar::new(),
        });
        let wake = Arc::clone(&park) as Arc<dyn Wake>;
        clock.subscribe(Arc::downgrade(&wake));
        (
            Self {
                job_id,
                session_id,
                output_path,
                clock,
                bound,
                cap,
                watch,
                shared,
                park,
            },
            stop,
        )
    }

    /// Builds the child's command, pipes its stdio, makes it the leader
    /// of its own group, and lists the group with the spawn: stdin is the
    /// lifeline the parent holds open, stdout is drained, and stderr is
    /// dropped. Returns the child's registration with it: only that token
    /// unlists that entry.
    pub(crate) fn spawn(
        &self,
        launch: &Launch,
        launched: &Launched,
    ) -> io::Result<(Child, Listing)> {
        let mut command = launch(launched);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let (child, listing) =
            support::group::spawn(&mut command).map_err(|source| match source {
                support::group::Error::Spawn(error) => error,
                source => io::Error::other(source.to_string()),
            })?;
        self.shared.set_pgid(child.id());
        Ok((child, listing))
    }

    /// One wait on the clock, taken only while the generation still
    /// equals the snapshot read before the loop's checks: a poke or clock
    /// move after the snapshot trips the check below instead of being
    /// slept through.
    fn park(&self, until: Option<Instant>, seen: u64) {
        let mut slot = Some(self.park.seq.lock().unwrap_or_else(PoisonError::into_inner));
        self.clock.wait_until(until, &mut |bound| {
            let Some(seq) = slot.take() else {
                return;
            };
            if *seq != seen {
                slot = Some(seq);
                return;
            }
            slot = Some(match bound {
                Some(timeout) => {
                    self.park
                        .cv
                        .wait_timeout(seq, timeout)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .park
                    .cv
                    .wait(seq)
                    .unwrap_or_else(PoisonError::into_inner),
            });
        });
    }

    /// Drives the delegate to its end and reports it once: the watch loop
    /// until the leader is reaped, the drain join, the fold, and the
    /// report. A surviving member's cleanup continues after the report and
    /// never delays the wake.
    ///
    /// The leader is reaped with `try_wait` under the list's lock, which
    /// is also where its group retires or its surviving member is
    /// killed: no `waitid` peek is needed, and none is used, because
    /// macOS blocks in `waitid` even with `WNOWAIT`.
    pub(crate) fn drive(self, mut child: Child, mut listing: Option<Listing>, finish: Finish) {
        // The lifeline: held open and never written to. End of file on it
        // means this parent is gone, so it stays open until the job ends.
        let _lifeline = child.stdin.take();
        let (drain_tx, drain_rx) = mpsc::channel();
        let wake = Arc::clone(&self.park) as Arc<dyn Wake>;
        match child.stdout.take() {
            Some(stdout) => {
                let drain_wake = Arc::clone(&wake);
                thread::spawn(move || drain(stdout, drain_tx, drain_wake.as_ref()));
            }
            None => {
                let _sent = drain_tx.send(None);
            }
        }
        let pgid = child.id();
        // The fold is shared with the watcher thread, which owns the
        // blocking socket read: this thread never waits on the socket
        // and always reaches the reap and the stop timer.
        let fold = Arc::new(Mutex::new(Fold::default()));
        let mut backoff = START_BACKOFF;
        let mut next_retry = self.clock.now();
        let mut exited_seen = false;
        loop {
            // Snapshotted before the checks below: a watcher poke or
            // clock move in between trips the wait at the end instead
            // of being slept through.
            let seen = self.park.generation();
            // One watch result, when the watcher finished one.
            match lock(&fold).result.take() {
                Some(WatchResult::Exited) => exited_seen = true,
                Some(WatchResult::Retry) => {
                    next_retry = later(self.clock.as_ref(), backoff);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
                None => {}
            }
            // The cap is judged on the real length of the log, on every
            // wake, whether or not a connection is up.
            if over_cap(&self.output_path, self.cap) {
                self.shared.halt(Termination::OutputCap);
            }
            // Reaped only under the list's lock: the reap cannot race a
            // signal to a retired group. Then the fold waits for the drain.
            if let Some(status) = group::reap_locked(&mut child, &mut listing) {
                let stdout = self.await_drain(&drain_rx);
                drop(_lifeline);
                let socket = lock(&fold).socket.clone();
                let (completed, finished) = outcome(
                    self.shared.reason(),
                    socket.as_ref(),
                    stdout.as_ref(),
                    status,
                    &self.job_id,
                );
                finish.report(completed, Some(finished));
                self.retire(&mut listing);
                return;
            }
            if let Some(kill_at) = self.shared.kill_at()
                && self.clock.now() >= kill_at
            {
                group::signal(pgid, Signal::KILL);
            }
            // A watch starts when one is due and none is running. The
            // watcher thread owns the blocking read; a second never
            // starts while one is outstanding.
            let launch = {
                let mut fold = lock(&fold);
                if exited_seen || fold.outstanding || self.clock.now() < next_retry {
                    false
                } else {
                    fold.outstanding = true;
                    true
                }
            };
            if launch {
                let watcher = Watcher {
                    watch: Arc::clone(&self.watch),
                    session_id: self.session_id.clone(),
                    fold: Arc::clone(&fold),
                    shared: Arc::clone(&self.shared),
                    output_path: self.output_path.clone(),
                    cap: self.cap,
                    wake: Arc::clone(&wake),
                };
                thread::spawn(|| watcher.run());
            }
            // The next deadline is the earliest of the three: a stale
            // retry or kill time never pushes it out.
            let outstanding = lock(&fold).outstanding;
            let due = park_due(
                later(self.clock.as_ref(), POLL),
                exited_seen,
                outstanding,
                next_retry,
                self.shared.kill_at(),
            );
            self.park(Some(due), seen);
        }
    }

    /// The drain has the leader's reap plus the bound to deliver its last
    /// line; past that the fold runs without it, and the drain thread is
    /// left to finish on its own.
    fn await_drain(&self, drain: &mpsc::Receiver<Option<FiberExited>>) -> Option<FiberExited> {
        let deadline = later(self.clock.as_ref(), self.bound);
        #[cfg(test)]
        if let Some(hook) = lock(&AFTER_DRAIN_DEADLINE).take() {
            hook();
        }
        loop {
            let seen = self.park.generation();
            match drain.try_recv() {
                Ok(line) => return line,
                Err(mpsc::TryRecvError::Disconnected) => return None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if self.clock.now() >= deadline {
                return None;
            }
            self.park(Some(deadline), seen);
        }
    }

    /// A surviving member was SIGKILLed at the reap and stays listed until
    /// its group is empty; the report above never waits for this. Past the
    /// stop bound a member still holding the group gets SIGKILL again:
    /// the reap's signal and this timer are what end every survivor
    /// (`docs/delegates.md`, "Lifetime"), and the listing stays until
    /// the group is empty, re-checked on each clock wake. Ends once this
    /// run's own registration is gone, whatever else shares the id.
    fn retire(&self, listing: &mut Option<Listing>) {
        while let Some(pgid) = listing.as_ref().map(Listing::pgid) {
            let seen = self.park.generation();
            if group::retire_if_empty(listing) {
                return;
            }
            if kill_due(self.shared.kill_at(), self.clock.now()) {
                group::signal(pgid, Signal::KILL);
            }
            self.park(Some(later(self.clock.as_ref(), POLL)), seen);
        }
    }
}

/// One watch call on its own thread: the blocking socket read lives
/// here, so the runner thread always reaches the reap, the cap checks
/// and the stop timer. Left to finish on its own once the child is
/// reaped: a watch that outlives the child ends at EOF.
struct Watcher {
    watch: Watch,
    session_id: SessionId,
    fold: Arc<Mutex<Fold>>,
    shared: Arc<Shared>,
    output_path: PathBuf,
    cap: u64,
    wake: Arc<dyn Wake>,
}

impl Watcher {
    fn run(self) {
        let result = (self.watch)(&self.session_id, &mut |envelope: &Envelope| {
            let exited = {
                let mut fold = lock(&self.fold);
                if !note_seq(&mut fold.last_seq, envelope.seq.map(|seq| seq.0)) {
                    return;
                }
                if envelope.kind == "fiber_exited" {
                    fiber_exited_of(&envelope.payload)
                } else {
                    None
                }
            };
            // Checked per delivered line, as the runner checks it per
            // wake: no lock is held across the halt.
            if over_cap(&self.output_path, self.cap) {
                self.shared.halt(Termination::OutputCap);
            }
            if let Some(exited) = exited {
                lock(&self.fold).socket = Some(exited);
            }
        });
        {
            let mut fold = lock(&self.fold);
            fold.outstanding = false;
            fold.result = Some(match result {
                Ok(Watched::Exited) => WatchResult::Exited,
                Ok(Watched::Closed) | Err(_) => WatchResult::Retry,
            });
        }
        // Wakes the runner's park at once: the result must not wait for
        // the next clock move.
        self.wake.wake();
    }
}

/// The runner's next park deadline: the earliest of the poll horizon,
/// a due retry, and the stop timer. A retry counts only while no watch
/// has finished and none is running, so a stale retry time never pulls
/// the deadline back (which would spin instead of parking) and an
/// exactly equal bound changes nothing.
pub(crate) fn park_due(
    poll_due: Instant,
    exited_seen: bool,
    outstanding: bool,
    next_retry: Instant,
    kill_at: Option<Instant>,
) -> Instant {
    let mut due = poll_due;
    if !exited_seen && !outstanding {
        due = due.min(next_retry);
    }
    if let Some(kill_at) = kill_at {
        due = due.min(kill_at);
    }
    due
}

/// Mints the delegate's session id: `s_` and 16 lowercase hex digits, drawn
/// once. The delegate's job id comes from the registry's
/// [`crate::registry::mint_job_id`]; the two must differ in prefix, so a
/// log line never names one where the other belongs.
pub(crate) fn mint_session_id() -> SessionId {
    SessionId(format!("s_{:016x}", RandomState::new().hash_one(())))
}

/// Whether the delegate's log has passed the cap. A log that does not
/// exist yet is empty.
fn over_cap(output_path: &PathBuf, cap: u64) -> bool {
    std::fs::metadata(output_path)
        .map(|meta| meta.len())
        .unwrap_or(0)
        > cap
}

/// The drain: reads stdout to EOF and keeps only the last `fiber_exited`
/// line, a pre-session one included. The pipe reaches EOF once every
/// writer in the group is gone.
fn drain(
    read: impl Read + Send + 'static,
    done: mpsc::Sender<Option<FiberExited>>,
    wake: &dyn Wake,
) {
    let mut kept = None;
    for line in BufReader::new(read).lines() {
        let Ok(line) = line else {
            break;
        };
        if let Some(exited) = fiber_exited_of_line(&line) {
            kept = Some(exited);
        }
    }
    let _sent = done.send(kept);
    wake.wake();
}

/// A stdout line's `fiber_exited`, when the line is one.
fn fiber_exited_of_line(line: &str) -> Option<FiberExited> {
    let envelope: Envelope = serde_json::from_str(line).ok()?;
    if envelope.kind != "fiber_exited" {
        return None;
    }
    fiber_exited_of(&envelope.payload)
}

/// A watch envelope's `fiber_exited`.
fn fiber_exited_of(payload: &serde_json::Map<String, serde_json::Value>) -> Option<FiberExited> {
    serde_json::from_value(serde_json::Value::Object(payload.clone())).ok()
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
