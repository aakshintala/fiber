//! Running a Fiber delegate: spawning the child session, draining its
//! stdout, watching its socket to its end, and folding how it ended
//! (`docs/delegates.md`, "Lifetime"). The runner makes no model call:
//! it supervises the child process while the tool answers as soon as the
//! spawn and the record are done. The runner's only answer is the end it
//! reports to the registry once the child is reaped.

use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::events::{EventKind, FiberExited};
use contract::jobs::Stop;
use contract::shapes::Usage;
use contract::{Envelope, JobId, PreSessionExit, SessionId};
use rustix::process::Signal;

use super::group;
use super::outcome::{Termination, outcome};
use crate::registry::Unreported;
use support::group::{GROUP_POLL, Listing};
use support::{clock::Parker, lock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobEventKind {
    SessionStarted,
    FiberExited,
    UsageRecorded,
}

fn classify_job_kind(kind: EventKind) -> Option<JobEventKind> {
    use contract::events::EventKind as Kind;

    match kind {
        Kind::SessionStarted => Some(JobEventKind::SessionStarted),
        Kind::FiberExited => Some(JobEventKind::FiberExited),
        Kind::UsageRecorded => Some(JobEventKind::UsageRecorded),
        Kind::FiberStarted
        | Kind::Rewound
        | Kind::Moved
        | Kind::TurnStarted
        | Kind::StepStarted
        | Kind::TurnCompleted
        | Kind::SteeringApplied
        | Kind::SteeringQueue
        | Kind::ShellCommand
        | Kind::DirectoryAdded
        | Kind::SessionNamed
        | Kind::Clients
        | Kind::SessionStatus
        | Kind::ContextAdded
        | Kind::AssistantMessageStarted
        | Kind::AssistantMessageDelta
        | Kind::AssistantMessageCompleted
        | Kind::TextCompleted
        | Kind::ToolCallArgumentsDelta
        | Kind::ReasoningStarted
        | Kind::ReasoningDelta
        | Kind::ReasoningCompleted
        | Kind::ToolCallRequested
        | Kind::ToolCallStarted
        | Kind::ToolCallDelta
        | Kind::ToolCallCompleted
        | Kind::PermissionRequested
        | Kind::PermissionResolved
        | Kind::InteractionRequested
        | Kind::InteractionResolved
        | Kind::RepositoryCodeOffered
        | Kind::RepositoryCodeResolved
        | Kind::QuotaNoticed
        | Kind::QuotaRearmed
        | Kind::RetryScheduled
        | Kind::Notice
        | Kind::PreambleBuilt
        | Kind::ModelChanged
        | Kind::OpeningMessage
        | Kind::InstructionFile
        | Kind::DateChanged
        | Kind::SkillsChanged
        | Kind::HandoffStarted
        | Kind::HandoffCompleted
        | Kind::SkillsResent
        | Kind::ContextNudged
        | Kind::ReviewerKept
        | Kind::McpServerFailed
        | Kind::McpServerReady
        | Kind::Reloaded
        | Kind::ExtensionsLoaded
        | Kind::ExtensionStateSet
        | Kind::ExtensionStateUnset
        | Kind::ExtensionUi
        | Kind::ExtensionMessage
        | Kind::ExtensionLog
        | Kind::ExtensionExec
        | Kind::ExtensionHello
        | Kind::ExtensionRequest
        | Kind::JobStarted
        | Kind::DelegateStarted
        | Kind::JobDelta
        | Kind::JobLine
        | Kind::DelegateFinished
        | Kind::JobCompleted
        | Kind::JobsPendingNotified
        | Kind::CommandAccepted
        | Kind::CommandRejected
        | Kind::Unknown(_) => None,
    }
}

/// Builds the child's command from its launch. The runner sets the stdio,
/// the process group and the lifeline around it.
pub type Launch = Arc<dyn Fn(&Launched) -> Command + Send + Sync>;

/// Watches the delegate's socket, calling `on_line` for each envelope
/// past the run's boundary until `fiber_exited` or EOF: the jobs-local
/// mirror of `doors::watch_after`. `main` supplies the watch over the
/// child's socket (`docs/delegates.md`, "Lifetime").
pub type Watch = Arc<
    dyn Fn(&SessionId, Option<u64>, &mut dyn FnMut(&Envelope)) -> io::Result<Watched> + Send + Sync,
>;

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

/// The worktree a run ends, or only reports when resuming.
#[derive(Debug, Clone)]
pub struct EndWorktree {
    /// The worktree directory.
    pub path: PathBuf,
    /// The recorded branch.
    pub branch: String,
    /// The initial commit, absent on resumed runs.
    pub base: Option<String>,
    /// Whether this runner owns removal.
    pub remove: bool,
}

/// A shared, idempotent release of the parent's worktree lock.
#[derive(Clone)]
pub struct WorktreeGuard(Arc<WorktreeHold>);

struct WorktreeHold(Mutex<Option<Box<dyn FnOnce() + Send>>>);

impl WorktreeGuard {
    /// Keeps the share alive until acknowledgement or failed-start cleanup.
    /// `release` drops the shared lock; it must not call back into this guard.
    pub fn new(release: impl FnOnce() + Send + 'static) -> Self {
        Self(Arc::new(WorktreeHold(Mutex::new(Some(Box::new(release))))))
    }

    /// Releases once, even when the watcher and runner race.
    pub fn release(&self) {
        // Keep the mutex until the fd is closed: teardown must not race
        // a watcher that took the callback but has not dropped its share.
        let mut held = lock(&self.0.0);
        if let Some(release) = held.take() {
            release();
        }
    }
}

impl Drop for WorktreeHold {
    fn drop(&mut self) {
        if let Some(release) = lock(&self.0).take() {
            release();
        }
    }
}

/// Creates the worktree and its parent's shared hold before child launch.
pub type CreateWorktree = Arc<
    dyn Fn(&Path, &SessionId) -> Result<(EndWorktree, WorktreeGuard), crate::JobsError>
        + Send
        + Sync,
>;

/// Inspects, optionally removes, and reports a worktree at run end.
pub type FinishWorktree =
    Arc<dyn Fn(EndWorktree) -> contract::events::FinishedWorktree + Send + Sync>;

pub(crate) struct WorktreeRun {
    pub(crate) end: EndWorktree,
    pub(crate) guard: Option<WorktreeGuard>,
    pub(crate) finish: FinishWorktree,
}

/// What the launcher receives: everything the child's argv needs.
pub struct Launched {
    /// The child's depth in the delegate tree.
    pub depth: u32,
    /// The delegate's session.
    pub session_id: SessionId,
    /// The delegate's job in its parent: its id for life.
    pub job_id: JobId,
    /// The parent session.
    pub parent: SessionId,
    /// The prompt, on argv.
    pub prompt: String,
    /// The parent's workspace, which the delegate shares.
    pub workspace: PathBuf,
    /// The parent's added folders at launch, in fold order: the child's
    /// argv carries each as `--added`, and a fresh spawn child records
    /// them as its own `directory_added` events (`docs/delegates.md`,
    /// "The tools"). Empty on a resume, whose own log already holds
    /// them.
    pub added: Vec<PathBuf>,
    /// The parent's pre-created worktree, absent without isolation.
    pub worktree: Option<contract::shapes::Worktree>,
    /// What this launch starts: a fresh child or a resume of the
    /// delegate's own session. A fresh launch and a resume render
    /// different argv, so the shape is an enum, never flags that can
    /// contradict each other.
    pub start: Start,
}

/// What a launch starts: a fresh child session or a new run of the same
/// job, continuing its own session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// A new child session: a spawn's or a fork's.
    Fresh(Fresh),
    /// A new run of the same job, continuing its own session.
    Resume,
}

/// A fresh child session: a spawn's, with its credential, or a fork's,
/// with its point. The credential lives on a spawn only, so a fork
/// carrying one is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fresh {
    /// A spawn: the resolved `provider/model[:level]` the child runs
    /// under, and the credential label it runs with, as a run
    /// override: `Some` on a new launch whose role names a stored
    /// label, `None` on a launch without one.
    Spawn {
        /// The resolved reference the child's `--model` names.
        model: String,
        /// The credential label, when the role names one.
        credential: Option<String>,
    },
    /// A fork: the resolved point it continues from, with the model
    /// reference the start records verbatim.
    Fork {
        /// The resolved point and its model reference.
        from: super::Forking,
    },
}

/// The first retry waits this long after a refused or dropped watch.
pub(crate) const START_BACKOFF: Duration = Duration::from_millis(50);

/// No retry waits longer than this, however often the watch fails.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(1);

/// How far ahead the runner parks at most: exit, cap and timer checks all
/// run on a wake at least this often.
const POLL: Duration = Duration::from_secs(1);

/// Test-only pause between computing the drain deadline and parking on it:
/// the test installs a one-shot hook, which the drain wait takes and runs
/// once. Empty means no effect and production never sets it.
#[cfg(test)]
static AFTER_DRAIN_DEADLINE: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

/// Test-only pause between taking the watch result and deciding the next
/// launch: the test installs a one-shot hook, which the drive loop takes
/// and runs past the lock, so the test can stage the next pause while
/// this one holds the runner. Empty means no effect and production never
/// sets it.
#[cfg(test)]
static AFTER_TAKE_RESULT: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

/// Test-only signal after the watcher stores each result: the test
/// installs a hook the watcher runs once the fold lands, so the test can
/// wait for the fold without sleeping. Empty means no effect and
/// production never sets it.
#[cfg(test)]
static ON_WATCH_FOLD: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

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

/// What the watch feeds the fold, shared with the watcher thread: the
/// last new `seq`, the socket's `fiber_exited` when it received one, and
/// the one-shot result of the running watch.
#[derive(Default)]
struct Fold {
    last_seq: Option<u64>,
    socket: Option<FiberExited>,
    /// The last usage `seq` forwarded to the loop, by the socket stream
    /// or the file backfill: a reconnect or backfill never forwards one
    /// at or before it twice.
    usage_seq: Option<u64>,
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

/// Forwards one tapped `usage_recorded` to the loop: the registry's
/// callback built from its weak self, carrying the delegate's session
/// with the line. The watcher invokes it outside every lock, in stream
/// order; a missing tap keeps the existing unwired behavior.
pub(crate) type UsageForward =
    Arc<dyn Fn(SessionId, contract::events::UsageRecorded) + Send + Sync>;

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
    /// Forwards each well-formed `usage_recorded` envelope to the loop,
    /// with the delegate's session. `None` keeps the existing unwired
    /// behavior.
    on_usage: Option<UsageForward>,
    deadline: Option<Instant>,
    /// The `seq` of the delegate log's last complete line before this
    /// run spawned: lines at or before it belong to an earlier run, so
    /// the watch skips them and the fold dedupes from there. `None` on
    /// the first run, which has no earlier line.
    after: Option<u64>,
    shared: Arc<Shared>,
    park: Arc<Parker>,
    /// When the watch callback last stat'ed the log for the cap, shared
    /// across watches: each watch runs on its own thread, but the poll
    /// interval spans them.
    last_cap_check: Arc<Mutex<Option<Instant>>>,
    worktree_run: Option<WorktreeRun>,
}

impl Runner {
    /// The runner and its stop. The stop goes to the record; the spawn
    /// sets the group the stop signals.
    #[allow(
        clippy::too_many_arguments,
        reason = "the runner carries the delegate's whole launch context"
    )]
    pub(crate) fn new(
        job_id: JobId,
        session_id: SessionId,
        output_path: PathBuf,
        clock: Arc<dyn Clock>,
        bound: Duration,
        cap: u64,
        watch: Watch,
        after: Option<u64>,
        timeout: Option<Duration>,
        on_usage: Option<UsageForward>,
    ) -> (Self, Stop) {
        let deadline = timeout.and_then(|timeout| clock.now().checked_add(timeout));
        let shared = Shared::new(Arc::clone(&clock), bound);
        let stopping = Arc::clone(&shared);
        let stop = Stop(Box::new(move || stopping.stop()));
        let park = Arc::new(Parker::new());
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
                on_usage,
                deadline,
                after,
                shared,
                park,
                last_cap_check: Arc::new(Mutex::new(None)),
                worktree_run: None,
            },
            stop,
        )
    }

    /// Attaches the fresh run's cleanup or a resumed run's report-only end.
    pub(crate) fn with_worktree(mut self, worktree_run: Option<WorktreeRun>) -> Self {
        self.worktree_run = worktree_run;
        self
    }

    /// Ends the worktree before reporting, also used after a failed spawn.
    pub(crate) fn end_worktree(&self) -> Option<contract::events::FinishedWorktree> {
        self.worktree_run.as_ref().map(|run| {
            if let Some(guard) = &run.guard {
                guard.release();
            }
            (run.finish)(EndWorktree {
                path: run.end.path.clone(),
                branch: run.end.branch.clone(),
                base: run.end.base.clone(),
                remove: run.end.remove,
            })
        })
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
        let (child, listing) = support::group::spawn(&mut command)?;
        self.shared.set_pgid(child.id());
        Ok((child, listing))
    }

    /// Drives the delegate to its end and reports it once: the watch loop
    /// until the leader is reaped, the drain join, the fold, and the
    /// report. A surviving member's cleanup continues after the report and
    /// never delays the wake.
    ///
    /// The Runner thread running `drive` owns the teardown: the
    /// `try_wait` reap below, and past the shutdown bound the group
    /// SIGKILL the stop sends, which ends the leader and any grandchild
    /// it spawned. The watcher thread never kills: it only owns the
    /// blocking socket read.
    ///
    /// The leader is reaped with `try_wait` under the list's lock, which
    /// is also where its group retires or its surviving member is
    /// killed: no `waitid` peek is needed, and none is used, because
    /// macOS blocks in `waitid` even with `WNOWAIT`.
    pub(crate) fn drive(self, mut child: Child, mut listing: Option<Listing>, finish: Unreported) {
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
        // and always reaches the reap and the stop timer. It dedupes
        // from the run's boundary, so a replayed earlier run's end never
        // stands in for this run's.
        let fold = Arc::new(Mutex::new(Fold {
            last_seq: self.after,
            ..Fold::default()
        }));
        let mut backoff = START_BACKOFF;
        let mut next_retry = self.clock.now();
        let mut exited_seen = false;
        // The drain's last line, taken once its sender answers: the
        // drain thread sends exactly one message at EOF and then its
        // sender drops, so any answer but `Empty` means stdout reached
        // EOF while the leader may still be unreaped. Kept here so the
        // reap below still folds it, and so the park at the end of the
        // loop can bound its wait once no stdout event can wake it.
        let mut drained: Option<Option<FiberExited>> = None;
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
            // Taken under the lock but run past it: the hook blocks
            // while the test stages the next pause, which needs the lock.
            #[cfg(test)]
            let hook = lock(&AFTER_TAKE_RESULT).take();
            #[cfg(test)]
            if let Some(hook) = hook {
                hook();
            }
            // The cap is judged on the real length of the log, on every
            // wake, whether or not a connection is up.
            if over_cap(&self.output_path, self.cap) {
                self.shared.halt(Termination::OutputCap);
            }
            // Reaped only under the list's lock: the reap cannot race a
            // signal to a retired group. Then the fold takes the drain's
            // line, waiting for it when none arrived yet.
            if drained.is_none() {
                match drain_rx.try_recv() {
                    Ok(line) => drained = Some(line),
                    Err(mpsc::TryRecvError::Disconnected) => drained = Some(None),
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
            if let Some(status) = group::reap_locked(&mut child, &mut listing) {
                // The socket misses a delegate that exits before the
                // first subscribe connects, when no watch ever ends: the
                // file replay after the reap still forwards its usage,
                // before the report wakes the waiter.
                Watcher::replay_usage(
                    &self.output_path,
                    self.after,
                    &fold,
                    &self.session_id,
                    &self.on_usage,
                );
                let stdout = match drained {
                    Some(line) => line,
                    None => self.await_drain(&drain_rx),
                };
                drop(_lifeline);
                let socket = lock(&fold).socket.clone();
                let (completed, mut finished) = outcome(
                    self.shared.reason(),
                    socket.as_ref(),
                    stdout.as_ref(),
                    status,
                    &self.job_id,
                );
                finished.worktree = self.end_worktree();
                finish.report(completed, Some(finished));
                self.retire(&mut listing);
                return;
            }
            if deadline_due(self.deadline, self.clock.now()) {
                self.shared.halt(Termination::Timeout);
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
                // The watcher may have folded between the take above and
                // this lock: a result older than this read must not decide
                // the launch, so the take and the decision share this lock
                // hold for every result kind, the same as the take above.
                match fold.result.take() {
                    Some(WatchResult::Exited) => exited_seen = true,
                    Some(WatchResult::Retry) => {
                        next_retry = later(self.clock.as_ref(), backoff);
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                    None => {}
                }
                if exited_seen || fold.outstanding || self.clock.now() < next_retry {
                    false
                } else {
                    fold.outstanding = true;
                    true
                }
            };
            if launch {
                let output_path = self.output_path.clone();
                let watcher = Watcher {
                    watch: Arc::clone(&self.watch),
                    session_id: self.session_id.clone(),
                    after: self.after,
                    fold: Arc::clone(&fold),
                    shared: Arc::clone(&self.shared),
                    cap: self.cap,
                    clock: Arc::clone(&self.clock),
                    last_check: Arc::clone(&self.last_cap_check),
                    stat: Arc::new({
                        let output_path = output_path.clone();
                        move || log_len(&output_path)
                    }),
                    output_path,
                    wake: Arc::clone(&wake),
                    on_usage: self.on_usage.clone(),
                    worktree_guard: self.worktree_run.as_ref().and_then(|run| run.guard.clone()),
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
                if self.shared.reason().is_none() {
                    self.deadline
                } else {
                    None
                },
            );
            // Past stdout EOF with the leader still unreaped, no event
            // wakes the runner when the leader exits, and a fake clock
            // never moves on its own: the park carries a real-time poll
            // cap, so the next iteration reaps. Until EOF the drain's own
            // wake ends the park, so the wait stays unbounded.
            let poll = drained.is_some().then_some(GROUP_POLL);
            self.park
                .park_polled(self.clock.as_ref(), Some(due), poll, seen);
        }
    }

    /// The drain has the leader's reap plus the bound to deliver its last
    /// line; past that the fold runs without it, and the drain thread is
    /// left to finish on its own.
    fn await_drain(&self, drain: &mpsc::Receiver<Option<FiberExited>>) -> Option<FiberExited> {
        let deadline = later(self.clock.as_ref(), self.bound);
        // Taken under the lock but run past it: the hook blocks while the
        // test's driver stages the 1 s move, which needs no lock here but
        // must not wait on one this thread holds.
        #[cfg(test)]
        let hook = lock(&AFTER_DRAIN_DEADLINE).take();
        #[cfg(test)]
        if let Some(hook) = hook {
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
            self.park.park(self.clock.as_ref(), Some(deadline), seen);
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
            self.park.park(
                self.clock.as_ref(),
                Some(later(self.clock.as_ref(), POLL)),
                seen,
            );
        }
    }
}

/// Reads the delegate log's length for the cap check: the real file's
/// length, or zero before it exists.
type Stat = Arc<dyn Fn() -> u64 + Send + Sync>;

/// One watch call on its own thread: the blocking socket read lives
/// here, so the runner thread always reaches the reap, the cap checks
/// and the stop timer. Left to finish on its own once the child is
/// reaped: a watch that outlives the child ends at EOF.
struct Watcher {
    watch: Watch,
    session_id: SessionId,
    /// The run's boundary, passed to every watch call.
    after: Option<u64>,
    fold: Arc<Mutex<Fold>>,
    shared: Arc<Shared>,
    cap: u64,
    clock: Arc<dyn Clock>,
    /// When the log was last stat'ed for the cap: envelopes inside one
    /// poll interval share the first stat, so a burst of lines costs one
    /// metadata read.
    last_check: Arc<Mutex<Option<Instant>>>,
    /// Reads the log's length. The runner wires the real file; tests
    /// count calls through here.
    stat: Stat,
    /// The delegate's log file. The runner wires the real path; the
    /// backfill replays it, so a delegate that exits before the first
    /// subscribe connects still forwards its usage.
    output_path: PathBuf,
    wake: Arc<dyn Wake>,
    /// Forwards each well-formed `usage_recorded` envelope to the loop.
    /// `None` keeps the existing unwired behavior.
    on_usage: Option<UsageForward>,
    worktree_guard: Option<WorktreeGuard>,
}

impl Watcher {
    fn run(self) {
        let after = self.after;
        let result = (self.watch)(&self.session_id, after, &mut |envelope: &Envelope| {
            // Adoption takes the child's share before its log opens. A full
            // subscription replays this durable line even after late attach.
            let kind = classify_job_kind(EventKind::from_name(&envelope.kind));
            if kind == Some(JobEventKind::SessionStarted)
                && envelope.session_id == self.session_id
                && let Some(guard) = &self.worktree_guard
            {
                guard.release();
            }
            let exited = {
                let mut fold = lock(&self.fold);
                if !note_seq(&mut fold.last_seq, envelope.seq.map(|seq| seq.0)) {
                    return;
                }
                if kind == Some(JobEventKind::FiberExited) {
                    fiber_exited_of(&envelope.payload)
                } else {
                    None
                }
            };
            // Taps each well-formed `usage_recorded` envelope in stream
            // order, outside the fold lock: malformed lines never reach
            // the loop, as the fold already ignores every non-`fiber_exited`
            // kind. The claim and the send are split: the claim runs
            // under the fold lock, so a file replay racing this stream
            // never forwards the same `seq` twice, whichever runs first.
            // The send runs after the claim returns, still with no lock
            // held across it.
            if kind == Some(JobEventKind::UsageRecorded)
                && let Some(forward) = &self.on_usage
                && let Ok(line) = serde_json::from_value::<contract::events::UsageRecorded>(
                    serde_json::Value::Object(envelope.payload.clone()),
                )
                && let Some(seq) = envelope.seq.map(|seq| seq.0)
                && Self::claim_usage(&self.fold, after, seq)
            {
                forward(self.session_id.clone(), line);
            }
            // The cap is stat'ed at most once per poll interval, read
            // from the session clock: envelopes inside one interval share
            // the first stat, so a burst of lines costs one metadata
            // read. A stat that sees the log past the cap still halts the
            // job at once, and no lock is held across the halt.
            let check = {
                let mut last = lock(&self.last_check);
                let now = self.clock.now();
                if cap_due(now, *last, POLL) {
                    *last = Some(now);
                    true
                } else {
                    false
                }
            };
            if check && over_len((self.stat)(), self.cap) {
                self.shared.halt(Termination::OutputCap);
            }
            if let Some(exited) = exited {
                lock(&self.fold).socket = Some(exited);
            }
        });
        // The socket misses a delegate that exits before the first
        // subscribe connects, so the file replays what the stream never
        // delivered, in `seq` order. Either path's lines are never sent
        // twice, whichever ran first.
        self.backfill_usage();
        {
            let mut fold = lock(&self.fold);
            fold.outstanding = false;
            fold.result = Some(match result {
                Ok(Watched::Exited) => WatchResult::Exited,
                Ok(Watched::Closed) | Err(_) => WatchResult::Retry,
            });
        }
        #[cfg(test)]
        if let Some(hook) = lock(&ON_WATCH_FOLD).as_ref() {
            hook();
        }
        // Wakes the runner's park at once: the result must not wait for
        // the next clock move.
        self.wake.wake();
    }

    /// Whether `seq` is a usage line the loop has not seen: past the
    /// run's boundary and past the last usage either path forwarded.
    /// The check and the mark share one fold-lock hold, so two replays
    /// racing on two threads never both win the same `seq`: the first
    /// claim forwards it, the second sees it as sent. The forward runs
    /// outside the lock, after this returns true.
    fn claim_usage(fold: &Mutex<Fold>, after: Option<u64>, seq: u64) -> bool {
        if after.is_some_and(|bound| seq <= bound) {
            return false;
        }
        let mut fold = lock(fold);
        if fold.usage_seq.is_some_and(|sent| seq <= sent) {
            return false;
        }
        fold.usage_seq = Some(seq);
        true
    }

    /// Replays the delegate's log file for `usage_recorded` lines the
    /// socket stream never delivered: past the run's boundary and the
    /// last forwarded `seq`, in `seq` order. A missing or torn file
    /// forwards nothing; the next watch or backfill sees the lines once
    /// the writer finishes them.
    fn backfill_usage(&self) {
        Self::replay_usage(
            &self.output_path,
            self.after,
            &self.fold,
            &self.session_id,
            &self.on_usage,
        );
    }

    /// The file replay both the watcher (after every watch ends) and the
    /// runner (after reaping the child) share: past `after` and the last
    /// forwarded `seq` in the shared fold, in `seq` order. The runner's
    /// call covers a delegate that exits before the first subscribe
    /// connects, when no watch ever ends. One fold-lock hold decides the
    /// whole file's fresh `seq`s and marks them sent before any forward,
    /// so two replays racing on two threads never forward one twice:
    /// the first hold wins every fresh `seq`, the second wins none, and
    /// the forwards run outside the lock in `seq` order.
    fn replay_usage(
        output_path: &std::path::Path,
        after: Option<u64>,
        fold: &Mutex<Fold>,
        session_id: &SessionId,
        on_usage: &Option<UsageForward>,
    ) {
        let Some(forward) = on_usage.clone() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(output_path) else {
            return;
        };
        let mut pending: Vec<(u64, contract::events::UsageRecorded)> = Vec::new();
        for raw in text.lines() {
            let Ok(envelope) = serde_json::from_str::<contract::Envelope>(raw) else {
                continue;
            };
            if classify_job_kind(EventKind::from_name(&envelope.kind))
                != Some(JobEventKind::UsageRecorded)
            {
                continue;
            }
            let Some(seq) = envelope.seq.map(|seq| seq.0) else {
                continue;
            };
            if after.is_some_and(|bound| seq <= bound) {
                continue;
            }
            let Ok(line) = serde_json::from_value(serde_json::Value::Object(envelope.payload))
            else {
                continue;
            };
            pending.push((seq, line));
        }
        pending.sort_by_key(|(seq, _)| *seq);
        let fresh: Vec<(u64, contract::events::UsageRecorded)> = {
            let mut fold = lock(fold);
            let mut fresh = Vec::new();
            for (seq, line) in pending {
                if fold.usage_seq.is_some_and(|sent| seq <= sent) {
                    continue;
                }
                fold.usage_seq = Some(seq);
                fresh.push((seq, line));
            }
            fresh
        };
        for (_, line) in fresh {
            forward(session_id.clone(), line);
        }
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
    deadline: Option<Instant>,
) -> Instant {
    let mut due = poll_due;
    if !exited_seen && !outstanding {
        due = due.min(next_retry);
    }
    if let Some(kill_at) = kill_at {
        due = due.min(kill_at);
    }
    if let Some(deadline) = deadline {
        due = due.min(deadline);
    }
    due
}

pub(crate) fn deadline_due(deadline: Option<Instant>, now: Instant) -> bool {
    deadline.is_some_and(|deadline| now >= deadline)
}

/// Mints a driver command id: `c_` and 16 lowercase hex digits, drawn
/// once. A call mints one id and sends that same id on every connection
/// it opens.
pub(crate) fn mint_command_id() -> String {
    support::mint("c_")
}

/// Mints the delegate's session id: `s_` and 16 lowercase hex digits, drawn
/// once. The delegate's job id comes from the registry's
/// [`crate::registry::mint_job_id`]; the two must differ in prefix, so a
/// log line never names one where the other belongs.
pub(crate) fn mint_session_id() -> SessionId {
    SessionId(support::mint("s_"))
}

/// Whether the watch callback stats the log now: the first envelope
/// after the watch starts, or one a full poll interval past the last
/// stat. The edge belongs to the new interval: an envelope exactly
/// `interval` past the last stat stats again.
pub(crate) fn cap_due(now: Instant, last: Option<Instant>, interval: Duration) -> bool {
    match last {
        None => true,
        Some(last) => now.saturating_duration_since(last) >= interval,
    }
}

/// Whether the delegate's log has passed the cap. A log that does not
/// exist yet is empty.
fn over_cap(output_path: &PathBuf, cap: u64) -> bool {
    over_len(log_len(output_path), cap)
}

/// The log's length in bytes. A missing log, or one whose metadata
/// cannot be read, counts as zero size.
fn log_len(output_path: &PathBuf) -> u64 {
    std::fs::metadata(output_path)
        .map(|meta| meta.len())
        .unwrap_or(0)
}

/// Whether `len` bytes have passed the cap: strictly past it, so a log
/// exactly at the cap still runs.
fn over_len(len: u64, cap: u64) -> bool {
    len > cap
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

/// A stdout line's `fiber_exited`, when the line is one: either the
/// normal session envelope, or the startup line a process prints when it
/// fails before any session exists. The startup shape carries no session
/// identity, so a line carrying one is judged as a session envelope or
/// not at all: a malformed envelope never launders into a startup
/// verdict, and a normal envelope never loses its usage or final
/// message to the startup branch.
fn fiber_exited_of_line(line: &str) -> Option<FiberExited> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let object = value.as_object()?;
    if object.contains_key("session_id") {
        let envelope: Envelope = serde_json::from_value(value).ok()?;
        if classify_job_kind(EventKind::from_name(&envelope.kind))
            != Some(JobEventKind::FiberExited)
        {
            return None;
        }
        return fiber_exited_of(&envelope.payload);
    }
    // No session identity: only the startup shape applies. It names its
    // own kind, so any other kind is no verdict. The normalized result
    // keeps the child's failure and exit code whole; nothing ran before
    // the failure, so usage is zero and there is no final message,
    // question or suspension.
    let startup: PreSessionExit = serde_json::from_value(value).ok()?;
    Some(FiberExited {
        exit_code: startup.payload.exit_code,
        usage: Usage::default(),
        final_message: None,
        error: Some(startup.payload.error),
        suspended_on: None,
        questions: None,
    })
}

/// A watch envelope's `fiber_exited`.
fn fiber_exited_of(payload: &serde_json::Map<String, serde_json::Value>) -> Option<FiberExited> {
    serde_json::from_value(serde_json::Value::Object(payload.clone())).ok()
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
