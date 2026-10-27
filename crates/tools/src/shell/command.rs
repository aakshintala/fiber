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
use super::drive::{LoopEnd, Phase, Run, pump_inner};
use super::moved::{CancelBridge, Moved};
use super::output::{Errors, OUTPUT_CAP, Shared, bump, lock, read_errors, read_output};
use super::spawn::scrub_env;
use super::spill::{Config as SpillConfig, Spill};
use super::tty;

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
    /// Bytes read before the call returned, or the spill they went to.
    pub output: Kept,
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

/// What a finished command kept: the whole output, or the spill it went to.
/// The spill rides boxed: it holds two file handles and three paths.
pub(crate) enum Kept {
    /// Bytes read before the call returned.
    Whole(Vec<u8>),
    /// The spill the output went to, past 1 MiB.
    Spilled(Box<Spill>),
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

/// A finished command, or one handed to a job. The move rides boxed:
/// it carries the drive state.
pub(crate) enum Ran {
    /// The command ended in this call.
    Finished(Finished),
    /// The command is still running and should become a job.
    Moved(Box<Moved>),
}

/// Runs `command` as `program -c` in `workdir` until the group is empty, the
/// timeout, or a cancel. With `tty` it runs in a pseudo-terminal instead of
/// pipes. `program` is `/bin/bash` or `sh`. Output streams
/// as `tool_call_delta` through `emit` while the call runs, text only
/// (`docs/tools.md`, "Shell", "Result and output"): the drive loop below
/// holds the emitter, so nothing emits after this returns.
#[allow(
    clippy::too_many_arguments,
    reason = "spawn, the deadline, the move policy and the spill are one call"
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
    spill: Option<SpillConfig>,
    matches: Option<&Path>,
    sandbox_settings: &super::sandbox::Settings,
    sandbox_workspace: &Path,
) -> Result<Ran, std::io::Error> {
    execute_inner(
        program,
        command,
        workdir,
        timeout,
        clock,
        cancel,
        emit,
        policy,
        jobs,
        spill,
        matches,
        sandbox_settings,
        sandbox_workspace,
        support::group::alive,
        Path::new("ps"),
        super::sandbox::install,
    )
}

/// The execution with the group probe and the member-query program
/// injected, so a test can force the probe to lag the reap and point the
/// listing at a fixture. Production passes the real probe and bare `ps`.
#[allow(
    clippy::too_many_arguments,
    reason = "spawn, the deadline, the move policy, the spill, the sandbox and the injected probe are one call"
)]
/// Marks a spawn failure as a sandbox-apply failure: the shell maps it to
/// `ShellError::SandboxApply` instead of `NotStarted`, so a policy the
/// kernel refused never reads as a missing program. Built in the parent,
/// where formatting and allocation are allowed.
#[derive(Debug)]
pub(crate) struct SandboxApply {
    /// Why the policy did not apply.
    pub cause: String,
}

impl std::fmt::Display for SandboxApply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "The sandbox could not be applied: {}.", self.cause)
    }
}

impl std::error::Error for SandboxApply {}

fn execute_inner(
    program: &Path,
    command: &str,
    workdir: &Path,
    timeout: Duration,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
    policy: MovePolicy,
    jobs: Option<&dyn Jobs>,
    spill: Option<SpillConfig>,
    matches: Option<&Path>,
    sandbox_settings: &super::sandbox::Settings,
    sandbox_workspace: &Path,
    alive: fn(u32) -> bool,
    ps: &Path,
    install: fn(&mut super::sandbox::Prepared) -> Result<(), i32>,
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
    if let Some(path) = matches {
        cmd.env(crate::search::matches::MATCHES_ENV, path);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = install;
    // A setup failure aborts spawn before it starts: the shell maps the
    // marker to a sandbox-apply failure, so the call never runs unconfined.
    // A disabled sandbox skips the policy and runs as before; an enabled
    // one that cannot apply fails instead of falling back to unconfined.
    #[cfg(target_os = "linux")]
    let prepared = if sandbox_settings.enabled {
        match super::sandbox::check_available(sandbox_settings, sandbox_workspace) {
            Err(unavailable) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    SandboxApply {
                        cause: unavailable.to_string(),
                    },
                ));
            }
            Ok(()) => match super::sandbox::prepare(sandbox_settings, sandbox_workspace) {
                Ok(prepared) => Some(prepared),
                Err(cause) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        SandboxApply { cause },
                    ));
                }
            },
        }
    } else {
        None
    };
    support::group::detach(&mut cmd, tty);
    // The close-on-exec sentinel: the child writes the raw errno here when
    // the policy does not apply, and the parent reads it after a spawn
    // failure to tell an apply failure from an exec failure.
    #[cfg(target_os = "linux")]
    let mut sentinel_parent: Option<std::os::unix::net::UnixStream> = None;
    #[cfg(target_os = "linux")]
    if let Some(prepared) = prepared {
        let (parent, child) = std::os::unix::net::UnixStream::pair()?;
        set_cloexec(&parent)?;
        set_cloexec(&child)?;
        sentinel_parent = Some(parent);
        let mut policy = prepared;
        #[allow(
            unsafe_code,
            reason = "installs the prepared Linux policy between fork and exec"
        )]
        // SAFETY: The closure runs between fork and exec, so it only runs
        // syscalls: Landlock `restrict_self` and seccomp `apply_filter` on a
        // policy prepared before fork. Success performs no allocation: the
        // ruleset is consumed through the borrowed policy and the seccomp
        // program only borrowed, never moved or freed. A failure writes the
        // raw errno to the sentinel, without formatting or allocation, and
        // returns it, so spawn aborts and the command never runs unconfined.
        // See docs/code-quality.md, "Unsafe".
        unsafe {
            use std::os::unix::process::CommandExt as _;
            cmd.pre_exec(move || {
                if let Err(code) = install(&mut policy) {
                    write_errno(&child, code);
                    return Err(std::io::Error::from_raw_os_error(code));
                }
                Ok(())
            });
        }
    }
    let (child, listing) = match support::group::spawn(&mut cmd) {
        Ok(started) => started,
        Err(error) => {
            #[cfg(target_os = "linux")]
            if let Some(parent) = sentinel_parent
                && let Some(marker) = read_sentinel(parent)
            {
                return Err(marker);
            }
            return Err(error.into());
        }
    };
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
        // Only a foreground call, a shell without jobs, or a resumed one
        // spills: a move on the first pass keeps today's buffer
        // (`docs/tools.md`, "Result and output").
        if matches!(policy, MovePolicy::Foreground | MovePolicy::Stay) {
            inner.spill_config = spill.clone();
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
        stream_carry: Vec::new(),
        timeout_at: start.checked_add(timeout),
        move_at: match policy {
            MovePolicy::Foreground => start.checked_add(MOVE_AFTER),
            MovePolicy::Stay
            | MovePolicy::Background
            | MovePolicy::Terminal
            | MovePolicy::Monitor => None,
        },
        members: None,
        pgid,
        listing: Some(Box::new(listing)),
        shared,
        job: None,
        feed: None,
    };
    let ended = pump_inner(&mut progress, policy, clock, cancel, emit, alive, ps);
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
        LoopEnd::Move(reason) => Ok(Ran::Moved(Box::new(Moved {
            reason,
            bridge: Some(bridge),
            progress,
            cap: OUTPUT_CAP,
            spill,
        }))),
    }
}

/// Marks a socket as close-on-exec, so the sentinel never leaks into the
/// command. Runs in the parent before fork, where syscalls may fail with an
/// `io::Error`.
#[cfg(target_os = "linux")]
fn set_cloexec(stream: &std::os::unix::net::UnixStream) -> std::io::Result<()> {
    use rustix::fd::AsFd as _;
    let fd = stream.as_fd();
    let flags = rustix::io::fcntl_getfd(&fd)?;
    rustix::io::fcntl_setfd(&fd, flags | rustix::io::FdFlags::CLOEXEC)?;
    Ok(())
}

/// Writes the raw errno to the sentinel: one `write` syscall, no
/// allocation, no formatting. Best effort: the parent reads whatever
/// arrived before EOF.
#[cfg(target_os = "linux")]
#[allow(
    unsafe_code,
    reason = "writes the raw errno to the sentinel between fork and exec"
)]
fn write_errno(child: &std::os::unix::net::UnixStream, code: i32) {
    use std::os::unix::io::AsRawFd as _;
    let bytes = code.to_ne_bytes();
    // SAFETY: `child` is our own just-created socket, open for writing; the
    // pointer and length name a stack array that outlives the call. One
    // syscall, no allocation.
    let _ = unsafe {
        libc::write(
            child.as_raw_fd(),
            bytes.as_ptr() as *const libc::c_void,
            bytes.len(),
        )
    };
}

/// Reads the sentinel after a spawn failure: four bytes mean the child
/// reported a policy-apply errno, and the failure is a sandbox-apply
/// failure; EOF means the child wrote nothing and the failure is an exec
/// failure. The child has exited, closing its end, so this cannot block:
/// no live process holds the write end.
#[cfg(target_os = "linux")]
fn read_sentinel(parent: std::os::unix::net::UnixStream) -> Option<std::io::Error> {
    use std::io::Read as _;
    let mut parent = parent;
    let mut bytes = [0u8; 4];
    let mut filled = 0;
    while filled < bytes.len() {
        match parent.read(&mut bytes[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => break,
        }
    }
    if filled == bytes.len() {
        let code = i32::from_ne_bytes(bytes);
        Some(std::io::Error::new(
            std::io::ErrorKind::Other,
            SandboxApply {
                cause: std::io::Error::from_raw_os_error(code).to_string(),
            },
        ))
    } else {
        None
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
