//! Spawning a command and waiting until its process group is empty, or stopping
//! it (`docs/tools.md`, "Running a command", "Stopping a command").

use std::fs::File;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::Event;
use contract::jobs::{Input, Jobs};
use contract::tool::Cancel;

use super::background::{MoveAsk, Step, running_step, wait_deadline};
use super::monitor::Feed;
use super::output::{
    Errors, JobStream, OUTPUT_CAP, Shared, bump, lock, read_errors, read_output, stream_output,
    stream_tail,
};
use super::tty;
use rustix::process::{Pid, Signal};

/// How often a group is re-checked while the shell has exited and members
/// remain. Picked, not measured.
const GROUP_POLL: Duration = Duration::from_millis(10);

/// SIGKILL follows SIGTERM by this long (`docs/tools.md`, "Stopping a command").
const GRACE: Duration = Duration::from_millis(800);

/// How long output is read after the group is empty or the stop
/// (`docs/tools.md`, "Stopping a command").
const DRAIN: Duration = Duration::from_secs(2);

/// A foreground command moves after this long
/// (`docs/tools.md`, "Moving to the background").
const MOVE_AFTER: Duration = Duration::from_secs(30);

/// Why Fiber stopped the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopKind {
    /// `timeout_ms` passed.
    Timeout,
    /// The call was cancelled.
    Cancel,
}

/// What the wait observed.
pub(crate) struct Finished {
    /// Bytes read before the call returned.
    pub output: Vec<u8>,
    /// The shell's status, once it was reaped.
    pub status: Option<ExitStatus>,
    /// Set when Fiber stopped the command.
    pub stop: Option<StopKind>,
    /// A stop ended with the pipe still open or the group still occupied.
    pub indeterminate: bool,
    /// A normal end whose pipe was still open at the drain bound.
    pub held_open: bool,
    /// Fiber sent SIGTERM or SIGKILL.
    pub sent_signal: bool,
    /// The job's output passed the cap, so Fiber stopped it.
    pub capped: bool,
    /// A monitor's flood stopped it.
    pub flooded: bool,
}

/// Whether the drive loop may hand the command to a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MovePolicy {
    /// Never moves. A shell without jobs, a job's own drive, and a command
    /// whose open failed.
    Stay,
    /// Moves when the shell exits with members left, or after 30 seconds.
    Foreground,
    /// Moves on the first pass. A shell exit with members still comes first.
    Background,
    /// As `Background`, with the command in a pseudo-terminal.
    Terminal,
    /// As `Background`, for a monitor: standard error on its own pipe.
    Monitor,
}

/// Why the command moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MoveReason {
    /// It had run for 30 seconds.
    AfterThirtySeconds,
    /// It was started with `run_in_background`.
    StartedInBackground,
    /// The `background` driver command asked for it.
    BackgroundCommand,
    /// The shell exited and left processes in the group.
    ShellExited {
        /// The shell's exit code.
        code: i32,
    },
}

/// A finished command, or one handed to a job.
pub(crate) enum Ran {
    /// The command ended in this call.
    Finished(Finished),
    /// The command is still running and should become a job.
    Moved(Moved),
}

/// Runs `command` as `program -c` in `workdir` until the group is empty, the
/// timeout, or a cancel. With `tty` it runs in a pseudo-terminal instead of
/// pipes. `program` is `/bin/bash` or `sh`. Output streams
/// as `tool_call_delta` through `emit` while the call runs, text only
/// (`docs/tools.md`, "Shell", "Result and output"): the drive loop below
/// holds the emitter, so nothing emits after this returns.
#[allow(
    clippy::too_many_arguments,
    reason = "spawn, the deadline, and the move policy are one call"
)]
pub(crate) fn execute(
    program: &Path,
    command: &str,
    workdir: &Path,
    timeout: Duration,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
    policy: MovePolicy,
    jobs: Option<&dyn Jobs>,
) -> Result<Ran, std::io::Error> {
    let tty = policy == MovePolicy::Terminal;
    let mut cmd = Command::new(program);
    cmd.arg("-c").arg(command).current_dir(workdir);
    let mut input = None;
    let mut errors = None;
    let read: Box<dyn Read + Send> = if tty {
        let terminal = tty::open()?;
        cmd.stdin(Stdio::from(terminal.secondary.try_clone()?))
            .stdout(Stdio::from(terminal.secondary.try_clone()?))
            .stderr(Stdio::from(terminal.secondary));
        input = Some(terminal.input);
        Box::new(terminal.reader)
    } else {
        let (read, write) = std::io::pipe()?;
        // A monitor's standard error is its own pipe, kept from its lines.
        let write_err = if policy == MovePolicy::Monitor {
            let (read_err, write_err) = std::io::pipe()?;
            errors = Some(read_err);
            write_err
        } else {
            write.try_clone()?
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(write))
            .stderr(Stdio::from(write_err));
        Box::new(read)
    };
    scrub_env(&mut cmd);
    detach(&mut cmd, tty);
    let child = cmd.spawn()?;
    // The parent drops every write end, or every secondary, so EOF arrives
    // when the last holder exits.
    drop(cmd);

    let pgid = child.id();
    let shared = Arc::new(Shared::default());
    {
        let mut inner = lock(&shared.inner);
        inner.input = input;
        if errors.is_some() {
            inner.lines = Some(Vec::new());
            inner.errors = Some(Errors::default());
        }
    }
    let reader = Arc::clone(&shared);
    thread::spawn(move || read_output(read, &reader));
    if let Some(read_err) = errors {
        let reader = Arc::clone(&shared);
        thread::spawn(move || read_errors(read_err, &reader));
    }
    let waiter = Arc::clone(&shared);
    thread::spawn(move || wait_child(child, &waiter));

    // The clock watches the command. The call's cancel watches a bridge, so
    // dropping the bridge after a move stops that cancel reaching the job.
    let bridge = CancelBridge::arm(&shared);
    clock.subscribe(Arc::downgrade(&(Arc::clone(&shared) as Arc<dyn Wake>)));
    cancel.subscribe(Arc::downgrade(&(Arc::clone(&bridge) as Arc<dyn Wake>)));

    // Registered before the drive loop parks and held until it returns, so
    // `background` finds the call for as long as it waits in the foreground.
    let registered = jobs
        .filter(|_| policy == MovePolicy::Foreground)
        .map(|jobs| {
            MoveAsk::register(
                jobs,
                Arc::downgrade(&(Arc::clone(&shared) as Arc<dyn Wake>)),
            )
        });
    let start = clock.now();
    let mut progress = Run {
        ask: registered.as_ref().map(|(ask, _)| Arc::clone(ask)),
        phase: Phase::Running,
        stop: None,
        sent_signal: false,
        seen_empty: false,
        streamed: 0,
        timeout_at: start.checked_add(timeout),
        move_at: match policy {
            MovePolicy::Foreground => start.checked_add(MOVE_AFTER),
            MovePolicy::Stay
            | MovePolicy::Background
            | MovePolicy::Terminal
            | MovePolicy::Monitor => None,
        },
        pgid,
        shared,
        job: None,
        feed: None,
    };
    let ended = pump(&mut progress, policy, clock, cancel, emit);
    // A call that left the foreground wait is no longer counted, whether it
    // finished or moved: `background` must not find it during the open.
    if let Some((ask, _)) = &registered {
        ask.end();
    }
    match ended {
        LoopEnd::Finished(finished) => {
            drop(bridge);
            Ok(Ran::Finished(finished))
        }
        LoopEnd::Move(reason) => Ok(Ran::Moved(Moved {
            reason,
            bridge: Some(bridge),
            progress,
            cap: OUTPUT_CAP,
        })),
    }
}

fn scrub_env(cmd: &mut Command) {
    cmd.env_clear();
    for (key, value) in std::env::vars_os() {
        // Non-interactive `bash -c` reads `BASH_ENV`. Dropping it is what
        // "reads no shell startup files" means for bash. `sh -c` reads none.
        if key == "BASH_ENV" {
            continue;
        }
        cmd.env(key, value);
    }
}

#[allow(
    unsafe_code,
    reason = "setsid between fork and exec, which CommandExt::pre_exec requires"
)]
fn detach(cmd: &mut Command, controlling_tty: bool) {
    // SAFETY: the closure runs in the child between fork and exec, where only
    // async-signal-safe calls are sound. It calls only setsid and, for a
    // terminal, the TIOCSCTTY ioctl on fd 0, both system calls. Neither
    // allocates; the error path builds an `io::Error` from a raw errno, which
    // does not allocate either. The child is single-threaded.
    unsafe {
        cmd.pre_exec(move || {
            rustix::process::setsid().map_err(raw_error)?;
            if controlling_tty {
                // The session has no terminal yet; the secondary, already
                // fd 0, becomes it.
                // SAFETY: fd 0 is open, the secondary `Command` dup'd onto it.
                let stdin = rustix::fd::BorrowedFd::borrow_raw(0);
                rustix::process::ioctl_tiocsctty(stdin).map_err(raw_error)?;
            }
            Ok(())
        });
    }
}

fn raw_error(err: rustix::io::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(err.raw_os_error())
}

fn wait_child(mut child: std::process::Child, shared: &Shared) {
    let status = child.wait().ok();
    let mut inner = lock(&shared.inner);
    inner.reaped = true;
    inner.status = status;
    bump(&mut inner);
    shared.cv.notify_all();
}

struct View {
    reaped: bool,
    /// The shell's exit code, once it was reaped.
    shell_exit: Option<i32>,
    eof: bool,
    seq: u64,
    cancelled: bool,
}

/// Running, then one stop (SIGTERM, SIGKILL 800 ms later), then a drain of
/// at most 2 s. A later cancel or timeout does not start a second stop.
#[derive(Clone, Copy)]
enum Phase {
    Running,
    Stopping { kill_at: Instant },
    Draining { until: Instant },
}

struct Run {
    phase: Phase,
    stop: Option<StopKind>,
    sent_signal: bool,
    seen_empty: bool,
    streamed: usize,
    timeout_at: Option<Instant>,
    move_at: Option<Instant>,
    pgid: u32,
    shared: Arc<Shared>,
    /// A job's `job_delta` lines. Set only on a job's drive.
    job: Option<JobStream>,
    /// A monitor's deliveries. Set only on a monitor's drive. Boxed, so a
    /// command that is not a monitor carries one pointer.
    feed: Option<Box<Feed>>,
    /// Set when `background` can reach this foreground call.
    ask: Option<Arc<MoveAsk>>,
}

/// The call's cancel reaches the command through this bridge. The cancel
/// holds it weakly and [`Moved`] holds the only strong reference, so
/// dropping it at a move stops the call's cancel reaching the job.
struct CancelBridge(Weak<Shared>);

impl CancelBridge {
    fn arm(shared: &Arc<Shared>) -> Arc<Self> {
        Arc::new(Self(Arc::downgrade(shared)))
    }
}

impl Wake for CancelBridge {
    fn wake(&self) {
        if let Some(shared) = self.0.upgrade() {
            shared.wake();
        }
    }
}

/// A command the drive loop has decided to move. The call thread opens the
/// job; the job thread continues from [`Moved::drive_job`].
pub(crate) struct Moved {
    pub reason: MoveReason,
    bridge: Option<Arc<CancelBridge>>,
    progress: Run,
    /// The job's output file stops at this many bytes. Tests set a small one.
    cap: u64,
}

impl Moved {
    pub(crate) fn pgid(&self) -> u32 {
        self.progress.pgid
    }

    /// The command's terminal input, once; `None` for a command on pipes.
    pub(crate) fn take_input(&self) -> Option<Input> {
        lock(&self.progress.shared.inner).input.take()
    }

    /// The command's output state, which the call waits on after the move.
    pub(super) fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.progress.shared)
    }

    /// Copies bytes already read into `file`, then points the reader at it.
    /// Both happen under the reader lock, so a chunk is not kept twice or lost.
    pub(crate) fn attach_output(&self, file: File) {
        lock(&self.progress.shared.inner).attach(file, self.cap);
    }

    /// Writes a monitor's held standard error to `file` and points its
    /// reader there; with no file, its standard error is dropped from now
    /// on. Nothing for any other command.
    pub(crate) fn attach_errors(&self, file: Option<File>) {
        if let Some(errors) = lock(&self.progress.shared.inner).errors.as_mut() {
            errors.attach(file, self.cap);
        }
    }

    /// Wakes this command when `cancel` fires. The job's stop uses it.
    pub(crate) fn arm(&self, cancel: &dyn Cancel) {
        let wake: Arc<dyn Wake> = self.progress.shared.clone();
        cancel.subscribe(Arc::downgrade(&wake));
    }

    /// The call's cancel no longer reaches this command.
    pub(crate) fn detach_call_cancel(&mut self) {
        self.bridge = None;
    }

    /// Runs the command to the end with no further move, on the job's
    /// cancel, which [`Moved::arm`] has subscribed.
    pub(crate) fn drive_job(
        mut self,
        clock: &dyn Clock,
        cancel: &dyn Cancel,
        stream: JobStream,
        feed: Option<Feed>,
    ) -> Finished {
        self.progress.job = Some(stream);
        self.progress.feed = feed.map(Box::new);
        self.run(MovePolicy::Stay, clock, cancel, &Silent)
    }

    /// Keeps running in the foreground. Used when the job could not be opened.
    pub(crate) fn resume(
        self,
        clock: &dyn Clock,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
    ) -> Finished {
        self.run(MovePolicy::Stay, clock, cancel, emit)
    }

    fn run(
        self,
        policy: MovePolicy,
        clock: &dyn Clock,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
    ) -> Finished {
        let Moved {
            bridge,
            mut progress,
            ..
        } = self;
        let _bridge = bridge;
        match pump(&mut progress, policy, clock, cancel, emit) {
            LoopEnd::Finished(finished) => finished,
            // `Stay` does not move. Finishing here keeps a bug from spinning.
            LoopEnd::Move(_) => {
                let view = view(&progress.shared, cancel);
                finish(
                    &progress.shared,
                    progress.stop,
                    progress.sent_signal,
                    progress.seen_empty,
                    view.eof,
                    emit,
                    progress.streamed,
                )
            }
        }
    }
}

struct Silent;

impl Emit for Silent {
    fn emit(&self, _event: &Event) {}
}

enum LoopEnd {
    Finished(Finished),
    Move(MoveReason),
}

fn exit_code_of(status: Option<ExitStatus>) -> i32 {
    status.and_then(|status| status.code()).unwrap_or(0)
}

fn pump(
    progress: &mut Run,
    policy: MovePolicy,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
) -> LoopEnd {
    loop {
        let view = view(&progress.shared, cancel);
        // Empty only counts after the shell is reaped, so its zombie is gone.
        if view.reaped && !group_alive(progress.pgid) {
            progress.seen_empty = true;
        }
        // Stopping or draining: a later `background` moves nothing.
        if !matches!(progress.phase, Phase::Running)
            && let Some(ask) = &progress.ask
        {
            ask.end();
        }
        // The drive loop holds the emitter and streams every pass, woken by
        // the reader on every chunk; the reader never holds it, so nothing
        // emits after this returns.
        progress.streamed = stream_output(&progress.shared, emit, progress.streamed);
        // Before the job's delta, so a delta shows its bytes were offered.
        if let Some(feed) = progress.feed.as_mut() {
            feed.pass(
                &progress.shared,
                clock,
                matches!(progress.phase, Phase::Running),
            );
        }
        if let Some(job) = progress.job.as_mut() {
            job.pass(&progress.shared, clock);
        }
        // A held `job_delta` wakes the park when it falls due.
        let held_until = progress.job.as_ref().and_then(JobStream::deadline);
        match progress.phase {
            Phase::Running => match running_step(
                policy,
                timeout_due(clock, progress.timeout_at),
                view.cancelled,
                progress.seen_empty,
                view.shell_exit,
                timeout_due(clock, progress.move_at),
                progress.ask.as_ref().is_some_and(|ask| ask.asked()),
            ) {
                Step::Stop(kind) => {
                    progress.stop = Some(kind);
                    progress.sent_signal =
                        send_term(progress.pgid, progress.sent_signal, progress.seen_empty);
                    progress.phase = Phase::Stopping {
                        kill_at: after(clock, GRACE),
                    };
                }
                Step::Drain => {
                    progress.phase = Phase::Draining {
                        until: after(clock, DRAIN),
                    };
                }
                Step::Move(reason) => return LoopEnd::Move(reason),
                Step::Park => park(
                    clock,
                    &progress.shared,
                    cancel,
                    sooner(
                        wait_deadline(policy, progress.timeout_at, progress.move_at),
                        held_until,
                    ),
                    view.reaped,
                    true,
                    view.seq,
                ),
            },
            Phase::Stopping { kill_at } => {
                if progress.seen_empty {
                    progress.phase = Phase::Draining {
                        until: after(clock, DRAIN),
                    };
                } else if clock.now() >= kill_at {
                    // One SIGKILL. The next state is the drain, so it is not sent again.
                    signal_group(progress.pgid, Signal::KILL);
                    progress.sent_signal = true;
                    progress.phase = Phase::Draining {
                        until: after(clock, DRAIN),
                    };
                } else {
                    park(
                        clock,
                        &progress.shared,
                        cancel,
                        sooner(Some(kill_at), held_until),
                        true,
                        false,
                        view.seq,
                    );
                }
            }
            Phase::Draining { until } => {
                // End-of-file alone is not the end: the group can still be
                // alive, and a stop is indeterminate only once the bound
                // passes with the pipe open or the group occupied.
                let settled = view.eof && progress.seen_empty;
                if settled || clock.now() >= until {
                    let mut finished = finish(
                        &progress.shared,
                        progress.stop,
                        progress.sent_signal,
                        progress.seen_empty,
                        view.eof,
                        emit,
                        progress.streamed,
                    );
                    // After `finish` the reader queues nothing more, so this
                    // is every byte the file holds, before the end is reported.
                    if let Some(job) = progress.job.as_mut() {
                        job.flush(&progress.shared);
                    }
                    if let Some(feed) = progress.feed.as_mut() {
                        feed.finish(&progress.shared, clock);
                        finished.flooded = feed.flooded();
                    }
                    return LoopEnd::Finished(finished);
                }
                park(
                    clock,
                    &progress.shared,
                    cancel,
                    sooner(Some(until), held_until),
                    poll_while_occupied(progress.seen_empty),
                    false,
                    view.seq,
                );
            }
        }
    }
}

fn view(shared: &Shared, cancel: &dyn Cancel) -> View {
    let inner = lock(&shared.inner);
    // Checked under this lock, after `subscribe`, so a cancel that lands in
    // between is visible (the waker takes the same lock before it notifies).
    View {
        reaped: inner.reaped,
        shell_exit: inner.reaped.then(|| exit_code_of(inner.status)),
        eof: inner.all_eof(),
        seq: inner.seq,
        // The cap stops a job as a stop does; the end reads `capped`.
        cancelled: cancel.is_cancelled() || inner.cap_fired,
    }
}

fn finish(
    shared: &Shared,
    stop: Option<StopKind>,
    sent_signal: bool,
    seen_empty: bool,
    eof: bool,
    emit: &dyn Emit,
    streamed: usize,
) -> Finished {
    let (output, status, capped) = {
        let mut inner = lock(&shared.inner);
        if !eof {
            // The reader blocks in its read until the last holder closes
            // the pipe. Discarding drops those bytes so they are not part
            // of the result.
            inner.discard = true;
        }
        (
            std::mem::take(&mut inner.output),
            inner.status,
            inner.cap_fired,
        )
    };
    // Tailed from this exact snapshot: the reader appends from here into a
    // fresh buffer the result never sees (discarded above while open), so
    // the concatenated delta texts equal the lossy result.
    stream_tail(&output, emit, streamed);
    let stopped = stop.is_some();
    Finished {
        output,
        status,
        stop,
        indeterminate: stopped && (!eof || !seen_empty),
        held_open: !stopped && !eof && seen_empty,
        sent_signal,
        capped,
        flooded: false,
    }
}

/// `wake_on_cancel` is set only while the command is still running. During
/// the grace and the drain the flag stays set, and treating it as a fresh
/// wake would spin.
pub(super) fn park(
    clock: &dyn Clock,
    shared: &Shared,
    cancel: &dyn Cancel,
    until: Option<Instant>,
    poll: bool,
    wake_on_cancel: bool,
    seen: u64,
) {
    // Taken before `wait_until`, and held until the condvar wait, so a wake
    // blocks on this lock instead of notifying nobody. `FnMut` cannot move
    // the guard out and back; the slot holds it across the one call.
    let mut slot = Some(lock(&shared.inner));
    clock.wait_until(until, &mut |bound| {
        let Some(guard) = slot.take() else {
            return;
        };
        let timeout = match (bound, poll) {
            (Some(bound), true) => Some(bound.min(GROUP_POLL)),
            (None, true) => Some(GROUP_POLL),
            (bound, false) => bound,
        };
        if already_woken(guard.seq, seen, wake_on_cancel, cancel.is_cancelled()) {
            slot = Some(guard);
            return;
        }
        slot = Some(match timeout {
            Some(timeout) => {
                shared
                    .cv
                    .wait_timeout(guard, timeout)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => shared
                .cv
                .wait(guard)
                .unwrap_or_else(PoisonError::into_inner),
        });
    });
}

fn send_term(pgid: u32, sent: bool, seen_empty: bool) -> bool {
    if suppress_term(sent, seen_empty) {
        return sent;
    }
    signal_group(pgid, Signal::TERM);
    true
}

/// The sequence moved, or a cancel should wake a run that is still going.
/// During the grace and the drain `wake_on_cancel` is false, so a cancel
/// that already fired does not spin the loop.
fn already_woken(seq: u64, seen: u64, wake_on_cancel: bool, cancelled: bool) -> bool {
    seq != seen || (wake_on_cancel && cancelled)
}

/// Poll the group while it may still be occupied. Once it has been seen
/// empty, no further check is needed.
fn poll_while_occupied(seen_empty: bool) -> bool {
    !seen_empty
}

/// Do not signal a group that was already signalled, or one seen empty.
fn suppress_term(sent: bool, seen_empty: bool) -> bool {
    sent || seen_empty
}

fn timeout_due(clock: &dyn Clock, at: Option<Instant>) -> bool {
    at.is_some_and(|at| clock.now() >= at)
}

/// The earlier of two optional instants.
fn sooner(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (one, other) => one.or(other),
    }
}

fn after(clock: &dyn Clock, delay: Duration) -> Instant {
    let now = clock.now();
    now.checked_add(delay).unwrap_or(now)
}

fn group_alive(pgid: u32) -> bool {
    // Waiting on group 1 or 0 would loop: nothing of this run is there.
    if refused_group(pgid) {
        return false;
    }
    pid(pgid).is_some_and(|pid| rustix::process::test_kill_process_group(pid).is_ok())
}

fn signal_group(pgid: u32, signal: Signal) {
    if refused_group(pgid) {
        return;
    }
    let Some(pid) = pid(pgid) else {
        return;
    };
    match rustix::process::kill_process_group(pid, signal) {
        Ok(()) | Err(_) => {}
    }
}

/// Group 1 or 0 is not a command's group: `kill(-1)` reaches every process
/// the user owns, and `kill(0)` this process's own group. No test passes such
/// an id to [`signal_group`], so a mutant of this check sends nothing; its own
/// test pins it.
fn refused_group(pgid: u32) -> bool {
    pgid <= 1
}

fn pid(raw: u32) -> Option<Pid> {
    Pid::from_raw(i32::try_from(raw).ok()?)
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
