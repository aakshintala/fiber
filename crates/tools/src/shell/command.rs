//! Spawning a command and waiting until its process group is empty, or stopping
//! it (`docs/tools.md`, "Running a command", "Stopping a command").

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::tool::Cancel;
use rustix::process::{Pid, Signal};

/// How often a group is re-checked while the shell has exited and members
/// remain. Picked, not measured.
const GROUP_POLL: Duration = Duration::from_millis(10);

/// SIGKILL follows SIGTERM by this long (`docs/tools.md`, "Stopping a command").
const GRACE: Duration = Duration::from_millis(800);

/// How long output is read after the group is empty or the stop
/// (`docs/tools.md`, "Stopping a command").
const DRAIN: Duration = Duration::from_secs(2);

/// Why Fiber stopped the command.
#[derive(Clone, Copy, PartialEq, Eq)]
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
}

/// Runs `command` as `program -c` in `workdir` until the group is empty, the
/// timeout, or a cancel. `program` is `/bin/bash` or `sh`.
pub(crate) fn execute(
    program: &Path,
    command: &str,
    workdir: &Path,
    timeout: Duration,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
) -> Result<Finished, std::io::Error> {
    let (read, write) = std::io::pipe()?;
    let write_err = write.try_clone()?;
    let mut cmd = Command::new(program);
    cmd.arg("-c")
        .arg(command)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(write))
        .stderr(Stdio::from(write_err));
    scrub_env(&mut cmd);
    detach(&mut cmd);
    let child = cmd.spawn()?;
    // The parent drops every write end so EOF arrives when the last holder exits.
    drop(cmd);

    let pgid = child.id();
    let shared = Arc::new(Shared::default());
    let reader = Arc::clone(&shared);
    thread::spawn(move || read_output(read, &reader));
    let waiter = Arc::clone(&shared);
    thread::spawn(move || wait_child(child, &waiter));
    Ok(drive(pgid, timeout, clock, cancel, &shared))
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
fn detach(cmd: &mut Command) {
    // SAFETY: the closure runs in the child between fork and exec, where only
    // async-signal-safe calls are sound. It calls only setsid, which is
    // async-signal-safe and does not allocate. The child is single-threaded.
    unsafe {
        cmd.pre_exec(|| match rustix::process::setsid() {
            Ok(_) => Ok(()),
            Err(err) => Err(std::io::Error::from_raw_os_error(err.raw_os_error())),
        });
    }
}

#[derive(Default)]
struct Inner {
    reaped: bool,
    status: Option<ExitStatus>,
    eof: bool,
    // debt: the whole output is held in memory until the call returns, #299's job output file or #435
    output: Vec<u8>,
    discard: bool,
    seq: u64,
}

#[derive(Default)]
struct Shared {
    inner: Mutex<Inner>,
    cv: Condvar,
}

impl Wake for Shared {
    fn wake(&self) {
        // The sequence moves under the same lock as the wait, so a cancel
        // or a clock advance that lands before `cv.wait` is still visible
        // when the waiter checks.
        let mut guard = lock(&self.inner);
        bump(&mut guard);
        self.cv.notify_all();
    }
}

fn read_output(mut read: impl Read, shared: &Shared) {
    let mut buf = [0_u8; 8192];
    loop {
        match read.read(&mut buf) {
            Ok(0) => {
                note_eof(shared);
                return;
            }
            Ok(n) => {
                let mut inner = lock(&shared.inner);
                if !inner.discard {
                    inner.output.extend_from_slice(buf.get(..n).unwrap_or(&[]));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                note_eof(shared);
                return;
            }
        }
    }
}

fn note_eof(shared: &Shared) {
    let mut inner = lock(&shared.inner);
    if inner.eof {
        return;
    }
    inner.eof = true;
    bump(&mut inner);
    shared.cv.notify_all();
}

fn wait_child(mut child: std::process::Child, shared: &Shared) {
    let status = child.wait().ok();
    let mut inner = lock(&shared.inner);
    inner.reaped = true;
    inner.status = status;
    bump(&mut inner);
    shared.cv.notify_all();
}

fn bump(inner: &mut Inner) {
    inner.seq = inner.seq.wrapping_add(1);
}

struct View {
    reaped: bool,
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

fn drive(
    pgid: u32,
    timeout: Duration,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    shared: &Arc<Shared>,
) -> Finished {
    let start = clock.now();
    let timeout_at = start.checked_add(timeout);
    let wake: Arc<dyn Wake> = shared.clone();
    let weak = Arc::downgrade(&wake);
    clock.subscribe(Weak::clone(&weak));
    cancel.subscribe(weak);

    let mut phase = Phase::Running;
    let mut stop = None;
    let mut sent_signal = false;
    let mut seen_empty = false;

    loop {
        let view = view(shared, cancel);
        // Empty only counts after the shell is reaped, so its zombie is gone.
        if view.reaped && !group_alive(pgid) {
            seen_empty = true;
        }
        match phase {
            Phase::Running => {
                if timeout_due(clock, timeout_at) {
                    stop = Some(StopKind::Timeout);
                    sent_signal = send_term(pgid, sent_signal, seen_empty);
                    phase = Phase::Stopping {
                        kill_at: after(clock, GRACE),
                    };
                } else if view.cancelled {
                    stop = Some(StopKind::Cancel);
                    sent_signal = send_term(pgid, sent_signal, seen_empty);
                    phase = Phase::Stopping {
                        kill_at: after(clock, GRACE),
                    };
                } else if seen_empty {
                    phase = Phase::Draining {
                        until: after(clock, DRAIN),
                    };
                } else {
                    park(
                        clock,
                        shared,
                        cancel,
                        timeout_at,
                        view.reaped,
                        true,
                        view.seq,
                    );
                }
            }
            Phase::Stopping { kill_at } => {
                if seen_empty {
                    phase = Phase::Draining {
                        until: after(clock, DRAIN),
                    };
                } else if clock.now() >= kill_at {
                    // One SIGKILL. The next state is the drain, so it is not sent again.
                    signal_group(pgid, Signal::KILL);
                    sent_signal = true;
                    phase = Phase::Draining {
                        until: after(clock, DRAIN),
                    };
                } else {
                    park(clock, shared, cancel, Some(kill_at), true, false, view.seq);
                }
            }
            Phase::Draining { until } => {
                // End-of-file alone is not the end: the group can still be
                // alive, and a stop is indeterminate only once the bound
                // passes with the pipe open or the group occupied.
                let settled = view.eof && seen_empty;
                if settled || clock.now() >= until {
                    return finish(shared, stop, sent_signal, seen_empty, view.eof);
                }
                park(
                    clock,
                    shared,
                    cancel,
                    Some(until),
                    poll_while_occupied(seen_empty),
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
        eof: inner.eof,
        seq: inner.seq,
        cancelled: cancel.is_cancelled(),
    }
}

fn finish(
    shared: &Shared,
    stop: Option<StopKind>,
    sent_signal: bool,
    seen_empty: bool,
    eof: bool,
) -> Finished {
    let (output, status) = {
        let mut inner = lock(&shared.inner);
        if !eof {
            // debt: the reader stays blocked until the last holder closes the pipe, #299 hands a held pipe to a job's output file
            inner.discard = true;
        }
        (std::mem::take(&mut inner.output), inner.status)
    };
    let stopped = stop.is_some();
    Finished {
        output,
        status,
        stop,
        indeterminate: stopped && (!eof || !seen_empty),
        held_open: !stopped && !eof && seen_empty,
        sent_signal,
    }
}

/// `wake_on_cancel` is set only while the command is still running. During
/// the grace and the drain the flag stays set, and treating it as a fresh
/// wake would spin.
fn park(
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

fn after(clock: &dyn Clock, delay: Duration) -> Instant {
    let now = clock.now();
    now.checked_add(delay).unwrap_or(now)
}

fn group_alive(pgid: u32) -> bool {
    // Group 1 or 0 is not a command. Signalling it reaches other processes,
    // and waiting on it would loop: nothing of this run is there.
    if pgid <= 1 {
        return false;
    }
    pid(pgid).is_some_and(|pid| rustix::process::test_kill_process_group(pid).is_ok())
}

fn signal_group(pgid: u32, signal: Signal) {
    // Group 1 or 0 is not a command. `kill(-1)` reaches every process the
    // user owns, and `kill(0)` this process's own group.
    if pgid <= 1 {
        return;
    }
    let Some(pid) = pid(pgid) else {
        return;
    };
    match rustix::process::kill_process_group(pid, signal) {
        Ok(()) | Err(_) => {}
    }
}

fn pid(raw: u32) -> Option<Pid> {
    Pid::from_raw(i32::try_from(raw).ok()?)
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
