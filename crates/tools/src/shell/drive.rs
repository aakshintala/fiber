//! Driving a command through running, stopping, draining and finishing.

use std::process::ExitStatus;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::emit::Emit;
use contract::tool::Cancel;
use rustix::process::Signal;

use super::background::{MoveAsk, Step, running_step, wait_deadline};
use super::command::{DRAIN, Finished, GRACE, GROUP_POLL, MovePolicy, MoveReason, StopKind};
use super::groups;
use super::monitor::Feed;
use super::output::{JobStream, Shared, lock, stream_output, stream_tail};
use super::process_group::{group_alive, send_term, signal_group};

pub(super) struct View {
    reaped: bool,
    /// The shell's exit code, once it was reaped.
    shell_exit: Option<i32>,
    pub(super) eof: bool,
    seq: u64,
    pub(super) cancelled: bool,
}

/// Running, then one stop (SIGTERM, SIGKILL 800 ms later), then a drain of
/// at most 2 s. A later cancel or timeout does not start a second stop.
#[derive(Clone, Copy)]
pub(super) enum Phase {
    Running,
    Stopping { kill_at: Instant },
    Draining { until: Instant },
}

pub(super) struct Run {
    pub(super) phase: Phase,
    pub(super) stop: Option<StopKind>,
    pub(super) sent_signal: bool,
    pub(super) seen_empty: bool,
    pub(super) streamed: usize,
    pub(super) timeout_at: Option<Instant>,
    pub(super) move_at: Option<Instant>,
    pub(super) pgid: u32,
    pub(super) shared: Arc<Shared>,
    /// A job's `job_delta` lines. Set only on a job's drive.
    pub(super) job: Option<JobStream>,
    /// A monitor's deliveries. Set only on a monitor's drive. Boxed, so a
    /// command that is not a monitor carries one pointer.
    pub(super) feed: Option<Box<Feed>>,
    /// Set when `background` can reach this foreground call.
    pub(super) ask: Option<Arc<MoveAsk>>,
}

pub(super) enum LoopEnd {
    Finished(Finished),
    Move(MoveReason),
}

pub(super) fn exit_code_of(status: Option<ExitStatus>) -> i32 {
    status.and_then(|status| status.code()).unwrap_or(0)
}

pub(super) fn pump(
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
        if let Some(job) = progress.job.as_mut() {
            job.pass(&progress.shared, clock);
        }
        // After the job's delta, before the park: the reader queues each
        // chunk for both under one lock, so every byte a delta carried is
        // offered before the drive thread parks again.
        if let Some(feed) = progress.feed.as_mut() {
            feed.pass(
                &progress.shared,
                clock,
                matches!(progress.phase, Phase::Running),
            );
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
                    groups::finished(progress.pgid, progress.seen_empty);
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

pub(super) fn view(shared: &Shared, cancel: &dyn Cancel) -> View {
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

pub(super) fn finish(
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

/// The sequence moved, or a cancel should wake a run that is still going.
/// During the grace and the drain `wake_on_cancel` is false, so a cancel
/// that already fired does not spin the loop.
pub(super) fn already_woken(seq: u64, seen: u64, wake_on_cancel: bool, cancelled: bool) -> bool {
    seq != seen || (wake_on_cancel && cancelled)
}

/// Poll the group while it may still be occupied. Once it has been seen
/// empty, no further check is needed.
pub(super) fn poll_while_occupied(seen_empty: bool) -> bool {
    !seen_empty
}

fn timeout_due(clock: &dyn Clock, at: Option<Instant>) -> bool {
    at.is_some_and(|at| clock.now() >= at)
}

/// The earlier of two optional instants.
pub(super) fn sooner(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (one, other) => one.or(other),
    }
}

fn after(clock: &dyn Clock, delay: Duration) -> Instant {
    let now = clock.now();
    now.checked_add(delay).unwrap_or(now)
}
