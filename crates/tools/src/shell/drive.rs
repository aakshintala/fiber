//! Driving a command through running, stopping, draining and finishing.

use std::path::Path;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::emit::Emit;
use contract::tool::Cancel;
use rustix::process::Signal;

use super::background::{GroupMembers, MoveAsk, Step, group_members, running_step, wait_deadline};
use super::command::{Finished, Kept, MovePolicy, MoveReason, StopKind};
use super::monitor::Feed;
use super::output::{JobStream, Shared, lock, stream_output, stream_rest};
use super::process_group::send_term;

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
    /// An incomplete UTF-8 sequence the stream queue's last take left.
    pub(super) stream_carry: Vec<u8>,
    pub(super) timeout_at: Option<Instant>,
    pub(super) move_at: Option<Instant>,
    /// The member listing's outcome, saved before a shell-exit move so the
    /// receipt reuses it instead of running `ps` a second time. `None`
    /// until the drive lists the group.
    pub(super) members: Option<GroupMembers>,
    pub(super) pgid: u32,
    /// The run's registration in the process-wide list, boxed so the run
    /// stays small. A run whose group was never seen empty drops it; the
    /// entry stays until the shared kill prunes it.
    pub(super) listing: Option<Box<support::group::Listing>>,
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
    pump_inner(
        progress,
        policy,
        clock,
        cancel,
        emit,
        support::group::alive,
        Path::new("ps"),
    )
}

/// The drive loop with the group probe and the member-query program
/// injected, so a test can force the probe to lag the reap and point the
/// listing at a fixture. Production passes the real probe and bare `ps`.
pub(super) fn pump_inner(
    progress: &mut Run,
    policy: MovePolicy,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
    alive: fn(u32) -> bool,
    ps: &Path,
) -> LoopEnd {
    loop {
        let view = view(&progress.shared, cancel);
        // Empty only counts after the shell is reaped, so its zombie is gone.
        if view.reaped && !alive(progress.pgid) {
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
        stream_output(&progress.shared, emit, &mut progress.stream_carry);
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
                        kill_at: after(clock, support::group::GRACE),
                    };
                }
                Step::Drain => {
                    progress.phase = Phase::Draining {
                        until: after(clock, support::group::DRAIN),
                    };
                }
                Step::Move(reason) => {
                    // A shell exit moves only on member evidence, never on
                    // a lagging alive probe. The first pass lists the
                    // group: no rows finishes in the foreground, while a
                    // listing that names members or never answers is saved
                    // and the loop re-reads the view, so a timeout or
                    // cancellation that became due during the listing wins
                    // before any handoff.
                    if !matches!(reason, MoveReason::ShellExited { .. }) {
                        return LoopEnd::Move(reason);
                    }
                    if progress.members.is_some() {
                        return LoopEnd::Move(reason);
                    }
                    match group_members(ps, progress.pgid, clock) {
                        GroupMembers::Empty => {
                            progress.seen_empty = true;
                        }
                        listed @ (GroupMembers::Members(_) | GroupMembers::Unavailable) => {
                            progress.members = Some(listed);
                        }
                    }
                }
                Step::Park => park(
                    clock,
                    &progress.shared,
                    cancel,
                    wait_deadline(policy, progress.timeout_at, progress.move_at)
                        .into_iter()
                        .chain(held_until)
                        .min(),
                    view.reaped,
                    true,
                    view.seq,
                ),
            },
            Phase::Stopping { kill_at } => {
                if progress.seen_empty {
                    progress.phase = Phase::Draining {
                        until: after(clock, support::group::DRAIN),
                    };
                } else if clock.now() >= kill_at {
                    // One SIGKILL. The next state is the drain, so it is not sent again.
                    match support::group::signal(progress.pgid, Signal::KILL) {
                        Ok(()) | Err(_) => {}
                    }
                    progress.sent_signal = true;
                    progress.phase = Phase::Draining {
                        until: after(clock, support::group::DRAIN),
                    };
                } else {
                    park(
                        clock,
                        &progress.shared,
                        cancel,
                        Some(kill_at).into_iter().chain(held_until).min(),
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
                    // The group leaves the list only once this run saw it
                    // empty; a group never seen empty stays until the
                    // shared kill prunes it.
                    if progress.seen_empty
                        && let Some(listing) = progress.listing.take()
                    {
                        support::group::live().unlist(*listing);
                    }
                    let mut finished = finish(
                        &progress.shared,
                        progress.stop,
                        progress.sent_signal,
                        progress.seen_empty,
                        view.eof,
                        emit,
                        &mut progress.stream_carry,
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
                    Some(until).into_iter().chain(held_until).min(),
                    !progress.seen_empty,
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
    carry: &mut Vec<u8>,
) -> Finished {
    let (output, status, capped) = {
        let mut inner = lock(&shared.inner);
        if !eof {
            // The reader blocks in its read until the last holder closes
            // the pipe. Discarding drops those bytes so they are not part
            // of the result.
            inner.discard = true;
        }
        // The spill leaves `Inner` with the result: nothing writes the
        // file after it is taken.
        let output = match inner.spill.take() {
            Some(spill) => Kept::Spilled(Box::new(spill)),
            None => Kept::Whole(std::mem::take(&mut inner.output)),
        };
        (output, inner.status, inner.cap_fired)
    };
    // The queue's rest goes out lossily, so below the cap the concatenated
    // delta texts equal the lossy whole output.
    stream_rest(shared, emit, carry);
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
    // The guard is held from the check into the condvar wait, so a wake
    // under the same lock is never missed.
    let _guard = support::clock::park(
        clock,
        until,
        poll.then_some(support::group::GROUP_POLL),
        &shared.cv,
        lock(&shared.inner),
        |guard| guard.seq != seen || (wake_on_cancel && cancel.is_cancelled()),
    );
}

fn timeout_due(clock: &dyn Clock, at: Option<Instant>) -> bool {
    at.is_some_and(|at| clock.now() >= at)
}

fn after(clock: &dyn Clock, delay: Duration) -> Instant {
    let now = clock.now();
    now.checked_add(delay).unwrap_or(now)
}
