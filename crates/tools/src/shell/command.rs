//! Spawning a command and waiting until its process group is empty, or stopping
//! it (`docs/tools.md`, "Running a command", "Stopping a command").

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::jobs::Jobs;
use contract::tool::Cancel;

use super::background::MoveAsk;
use super::drive::{LoopEnd, Phase, Run, pump};
use super::groups;
use super::moved::CancelBridge;
use super::output::{Errors, OUTPUT_CAP, Shared, bump, lock, read_errors, read_output};
use super::spawn::{detach, scrub_env};
use super::tty;

pub(super) use super::drive::park;
#[cfg(test)]
pub(super) use super::drive::{sooner, view};
pub(super) use super::moved::Moved;
#[cfg(test)]
pub(super) use super::output::JobStream;
pub(super) use super::process_group::{group_alive, signal_group};

/// How often a group is re-checked while the shell has exited and members
/// remain. Picked, not measured.
pub(super) const GROUP_POLL: Duration = Duration::from_millis(10);

/// SIGKILL follows SIGTERM by this long (`docs/tools.md`, "Stopping a command").
pub(super) const GRACE: Duration = Duration::from_millis(800);

/// How long output is read after the group is empty or the stop
/// (`docs/tools.md`, "Stopping a command").
pub(super) const DRAIN: Duration = Duration::from_secs(2);

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
    let child = groups::spawn(&mut cmd)?;
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

fn wait_child(mut child: std::process::Child, shared: &Shared) {
    let status = child.wait().ok();
    let mut inner = lock(&shared.inner);
    inner.reaped = true;
    inner.status = status;
    bump(&mut inner);
    shared.cv.notify_all();
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
