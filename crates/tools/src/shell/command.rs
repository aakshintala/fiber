//! Spawning a command and waiting until its process group is empty, or stopping
//! it (`docs/tools.md`, "Running a command", "Stopping a command").

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::jobs::Jobs;
use contract::tool::Cancel;

use super::background::MoveAsk;
use super::drive::{LoopEnd, Phase, Run, pump_inner};
use super::moved::{CancelBridge, Moved};
use super::output::{Errors, OUTPUT_CAP, PacedStream, Shared, bump, lock};
use super::sandbox::protected::{
    Guard as ProtectionGuard, ProtectedPaths, Report as ProtectedReport,
};
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

pub(super) fn spawn_stdin(policy: MovePolicy) -> contract::keeper::HostStdin {
    if policy == MovePolicy::Terminal {
        contract::keeper::HostStdin::Pty
    } else {
        contract::keeper::HostStdin::Null
    }
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
    /// The command ended in this call, with the protected-path report of
    /// its end: `None` when no confined transaction ran.
    Finished(Finished, Option<ProtectedReport>),
    /// The command is still running and should become a job. The guard
    /// rides in the move and ends with the job.
    Moved(Box<Moved>),
    /// The emitter declined the start (`Emit::starting`): nothing was
    /// spawned, and the call is cancelled before it started.
    Declined,
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
    timeout: Option<Duration>,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
    policy: MovePolicy,
    jobs: Option<&dyn Jobs>,
    spill: Option<SpillConfig>,
    matches: Option<&Path>,
    sandbox_settings: &super::sandbox::Settings,
    sandbox_workspace: &Path,
    protection: Option<&ProtectedPaths>,
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
        protection,
        support::group::alive,
        Path::new("ps"),
        super::sandbox::install,
    )
}

/// The execution with the group probe and the member-query program
/// injected, so a test can force the probe to lag the reap and point the
/// listing at a fixture. Production passes the real probe and bare `ps`.
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
    // This private marker is downcast to recover its cause; the user-facing
    // message is built by `ShellError::SandboxApply`, not this formatter.
    #[cfg_attr(false, mutants::skip)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "The sandbox could not be applied: {}.", self.cause)
    }
}

impl std::error::Error for SandboxApply {}

/// Which drive loop `finish_spawned` runs: the existing pipeline, or the
/// macOS Seatbelt one gated on the confinement marker. A gated call hands
/// nothing off before its marker is observed or the wrapper exits; a
/// missing marker at the end fails closed instead of ending normally.
pub(super) enum Drive {
    /// The existing pipeline: one drive pass, then the end.
    Plain,
    /// The Seatbelt pipeline: every transfer waits for the marker.
    #[cfg(target_os = "macos")]
    Gated(Arc<super::sandbox::MarkerWatch>),
}

/// The streams one spawn holds open for the shared driver below.
pub struct SpawnStreams {
    /// The command's terminal input, if any.
    pub input: Option<contract::jobs::Input>,
    /// Standard error, kept separate for monitor jobs.
    pub errors: Option<std::io::PipeReader>,
    /// Standard output, or a combined ordinary stream.
    pub read: Box<dyn Read + Send>,
}

/// Takes the protection guard for a confined Linux spawn: the baseline is
/// taken or reused now, and the guard owns the end-of-call compare, restore
/// and report. A baseline that cannot be read or recorded fails the call
/// closed before spawn, as a sandbox-apply failure. Unconfined spawns, and
/// builds without the Linux sandbox, take no guard.
#[cfg(target_os = "linux")]
pub(super) fn acquire_protection(
    sandbox_settings: &super::sandbox::Settings,
    sandbox_workspace: &Path,
    protection: Option<&ProtectedPaths>,
) -> Result<Option<ProtectionGuard>, std::io::Error> {
    match protection {
        Some(handle) if sandbox_settings.enabled => handle
            .begin(sandbox_workspace)
            .map(|begin| Some(begin.guard))
            .map_err(|error| {
                std::io::Error::other(SandboxApply {
                    cause: error.to_string(),
                })
            }),
        Some(_) | None => Ok(None),
    }
}

/// No guard off Linux: no confined call runs there.
// Mutants run on Linux, where this stub is not compiled.
#[cfg_attr(false, mutants::skip)]
#[cfg(not(target_os = "linux"))]
pub(super) fn acquire_protection(
    sandbox_settings: &super::sandbox::Settings,
    sandbox_workspace: &Path,
    protection: Option<&ProtectedPaths>,
) -> Result<Option<ProtectionGuard>, std::io::Error> {
    let _ = (sandbox_settings, sandbox_workspace, protection);
    Ok(None)
}

/// Opens the terminal or the pipes for `cmd`, by move policy.
fn open_streams(cmd: &mut Command, policy: MovePolicy) -> std::io::Result<SpawnStreams> {
    let tty = policy == MovePolicy::Terminal;
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
    Ok(SpawnStreams {
        input,
        errors,
        read,
    })
}

/// Drives a spawned child through the existing execution pipeline. The
/// Seatbelt spawn shares it; only its command construction, marker
/// stripping, and transfer gating differ, carried by `drive`.
#[allow(
    clippy::too_many_arguments,
    reason = "spawn, the deadline, the move policy, the spill and the drive gate are one call"
)]
pub(super) fn finish_spawned(
    spawned: super::hosted::Waiter,
    timeout: Option<Duration>,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
    policy: MovePolicy,
    jobs: Option<&dyn Jobs>,
    spill: Option<SpillConfig>,
    protection: Option<ProtectionGuard>,
    alive: fn(u32) -> bool,
    ps: &Path,
    drive: Drive,
) -> Result<Ran, std::io::Error> {
    let pgid = spawned.pgid();
    let identity = spawned.identity();
    let shared = Arc::new(Shared::default());
    // The marker observation wakes the drive loop parked in
    // `drive_confined`: the wake bumps under the same lock the park
    // holds from check into wait, so it is never missed, and the loop
    // re-checks the observed marker before handing the call off.
    #[cfg(target_os = "macos")]
    if let Drive::Gated(watch) = &drive {
        let waked = Arc::clone(&shared);
        watch.set_wake(Arc::new(move || {
            let mut inner = lock(&waked.inner);
            bump(&mut inner);
            drop(inner);
            waked.cv.notify_all();
        }));
    }
    {
        let mut inner = lock(&shared.inner);
        if spawned.monitor() {
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
    let (listing, hosted) = spawned.watch(&shared);

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
        timeout_at: if hosted.is_some() {
            None
        } else {
            timeout.and_then(|timeout| start.checked_add(timeout))
        },
        move_at: match policy {
            MovePolicy::Foreground => start.checked_add(MOVE_AFTER),
            MovePolicy::Stay
            | MovePolicy::Background
            | MovePolicy::Terminal
            | MovePolicy::Monitor => None,
        },
        members: None,
        identity,
        pgid,
        listing: listing.map(Box::new),
        hosted,
        shared,
        job: None,
        // A call that moves to a job flushes this stream first, and the
        // job's own stream carries the output from there.
        paced: emit.paced().then(PacedStream::call),
        feed: None,
    };
    let ended = match &drive {
        Drive::Plain => pump_inner(&mut progress, policy, clock, cancel, emit, alive, ps),
        #[cfg(target_os = "macos")]
        Drive::Gated(watch) => drive_confined(
            &mut progress,
            policy,
            clock,
            cancel,
            emit,
            alive,
            ps,
            Some(&**watch),
        ),
    };
    // A call that left the foreground wait is no longer counted, whether it
    // finished or moved: `background` must not find it during the open.
    if let Some((ask, _)) = &registered {
        ask.end();
    }
    // A gated call whose wrapper never showed its marker fails closed
    // instead of ending normally: nothing proved the sandbox applied, so no
    // end — finished or moved — may carry the call's output as confined.
    #[cfg(target_os = "macos")]
    if let Drive::Gated(watch) = &drive {
        require_confined_marker(watch, &progress)?;
    }
    match ended {
        LoopEnd::Finished(finished) => {
            drop(bridge);
            // The output is collected: compare, restore and report now, so
            // the foreground result carries the call's end.
            let report = protection.map(ProtectionGuard::finish);
            Ok(Ran::Finished(finished, report))
        }
        LoopEnd::Move(reason) => Ok(Ran::Moved(Box::new(Moved {
            reason,
            bridge: Some(bridge),
            progress,
            cap: OUTPUT_CAP,
            spill,
            protection,
        }))),
    }
}

/// Fails closed if a macOS wrapper ended without proving that Seatbelt
/// applied. Isolated from the shared pipeline so Linux mutation tests can
/// exempt this macOS-only behavior without exempting shared execution.
#[cfg(target_os = "macos")]
// mutants run on Linux, where this helper is not compiled.
#[cfg_attr(false, mutants::skip)]
fn require_confined_marker(
    watch: &super::sandbox::MarkerWatch,
    progress: &Run,
) -> Result<(), std::io::Error> {
    if watch.observed() {
        return Ok(());
    }
    let status = lock(&progress.shared.inner).status;
    Err(std::io::Error::other(SandboxApply {
        cause: super::sandbox::apply_cause(watch, status),
    }))
}

#[allow(
    clippy::too_many_arguments,
    reason = "spawn, the deadline, the move policy, the spill, the sandbox and the injected probe are one call"
)]
fn execute_inner(
    program: &Path,
    command: &str,
    workdir: &Path,
    timeout: Option<Duration>,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
    policy: MovePolicy,
    jobs: Option<&dyn Jobs>,
    spill: Option<SpillConfig>,
    matches: Option<&Path>,
    sandbox_settings: &super::sandbox::Settings,
    sandbox_workspace: &Path,
    protection: Option<&ProtectedPaths>,
    alive: fn(u32) -> bool,
    ps: &Path,
    install: fn(&mut super::sandbox::Prepared) -> Result<(), i32>,
) -> Result<Ran, std::io::Error> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (install, sandbox_settings, sandbox_workspace);
    // On macOS the policy applies through the `sandbox-exec` wrapper spawn,
    // with no `pre_exec`: only `install` needs explicit consumption.
    #[cfg(target_os = "macos")]
    let _ = install;
    // A setup failure aborts spawn before it starts: the shell maps the
    // marker to a sandbox-apply failure, so the call never runs unconfined.
    // A disabled sandbox skips the policy and runs as before; an enabled
    // one that cannot apply fails instead of falling back to unconfined.
    // The confined launch below builds the Seatbelt command and shares the
    // existing execution pipeline, so a test can drive the same helper
    // with an intentionally invalid profile.
    #[cfg(target_os = "macos")]
    if sandbox_settings.enabled {
        match super::sandbox::check_available(sandbox_settings, sandbox_workspace) {
            Err(unavailable) => {
                return Err(std::io::Error::other(SandboxApply {
                    cause: unavailable.to_string(),
                }));
            }
            Ok(()) => match super::sandbox::prepare(sandbox_settings, sandbox_workspace, None) {
                Ok(prepared) => {
                    return execute_confined(
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
                        alive,
                        ps,
                        &prepared.profile,
                        &prepared.bindings,
                    );
                }
                Err(cause) => {
                    return Err(std::io::Error::other(SandboxApply {
                        cause: cause.to_string(),
                    }));
                }
            },
        }
    }
    // The protection guard is taken before the confined Linux spawn: a
    // baseline that cannot be read or recorded stops the call here, so no
    // confined command runs unprotected. Unconfined spawns take none.
    let guard = acquire_protection(sandbox_settings, sandbox_workspace, protection)?;
    let spec = contract::keeper::Spawn {
        id: String::new(),
        program: program.to_path_buf(),
        args: vec!["-c".into(), command.into()],
        cwd: workdir.to_path_buf(),
        env: matches
            .map(|path| {
                vec![(
                    crate::search::matches::MATCHES_ENV.into(),
                    path.to_string_lossy().into_owned(),
                )]
            })
            .unwrap_or_default(),
        stdin: spawn_stdin(policy),
        timeout_ms: None,
        output_cap: OUTPUT_CAP,
        sandbox: None,
    };
    // Every check that can refuse the call has run: the person's command
    // starts only if its start is recorded (`Emit::starting`).
    if !emit.starting() {
        drop(guard);
        return Ok(Ran::Declined);
    }
    let (child, listing, streams) = build_inner(
        &spec,
        policy,
        sandbox_settings,
        sandbox_workspace,
        guard.as_ref().map(|guard| guard.dir.baseline.as_path()),
        install,
    )?;
    finish_spawned(
        super::hosted::Waiter::Child {
            child,
            listing,
            streams,
        },
        timeout,
        clock,
        cancel,
        emit,
        policy,
        jobs,
        spill,
        guard,
        alive,
        ps,
        Drive::Plain,
    )
}

/// Builds a keeper process with explicit environment, streams and confinement.
pub(crate) fn build(
    spec: &contract::keeper::Spawn,
) -> std::io::Result<(std::process::Child, support::group::Listing, SpawnStreams)> {
    let empty = super::sandbox::Settings {
        enabled: false,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: std::path::PathBuf::new(),
    };
    let settings = spec
        .sandbox
        .as_ref()
        .map(|sandbox| super::sandbox::Settings {
            enabled: sandbox.enabled,
            allow_write: sandbox.allow_write.clone(),
            deny_read: sandbox.deny_read.clone(),
            fiber_home: sandbox.fiber_home.clone(),
        });
    let workspace = spec
        .sandbox
        .as_ref()
        .map_or(spec.cwd.as_path(), |sandbox| sandbox.workspace.as_path());
    let baseline = spec
        .sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.baseline.as_deref());
    let policy = match spec.stdin {
        contract::keeper::HostStdin::Pty => MovePolicy::Terminal,
        contract::keeper::HostStdin::Null => MovePolicy::Monitor,
    };
    build_inner(
        spec,
        policy,
        settings.as_ref().unwrap_or(&empty),
        workspace,
        baseline,
        super::sandbox::install,
    )
}

fn build_inner(
    spec: &contract::keeper::Spawn,
    policy: MovePolicy,
    sandbox_settings: &super::sandbox::Settings,
    sandbox_workspace: &Path,
    baseline: Option<&Path>,
    install: fn(&mut super::sandbox::Prepared) -> Result<(), i32>,
) -> std::io::Result<(std::process::Child, support::group::Listing, SpawnStreams)> {
    let tty = policy == MovePolicy::Terminal;
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args).current_dir(&spec.cwd);
    #[cfg(target_os = "macos")]
    if sandbox_settings.enabled {
        super::sandbox::check_available(sandbox_settings, sandbox_workspace).map_err(|error| {
            std::io::Error::other(SandboxApply {
                cause: error.to_string(),
            })
        })?;
        let prepared = super::sandbox::prepare(sandbox_settings, sandbox_workspace, baseline)
            .map_err(|error| {
                std::io::Error::other(SandboxApply {
                    cause: error.to_string(),
                })
            })?;
        cmd = Command::new(super::sandbox::SANDBOX_EXEC);
        cmd.arg("-p").arg(&prepared.profile);
        for (name, value) in &prepared.bindings {
            cmd.arg("-D").arg(format!("{name}={value}"));
        }
        cmd.arg("--")
            .arg(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (install, baseline, sandbox_settings, sandbox_workspace);
    let streams = open_streams(&mut cmd, policy)?;
    scrub_env(&mut cmd);
    cmd.envs(spec.env.iter().map(|(key, value)| (key, value)));
    #[cfg(target_os = "linux")]
    let prepared = if sandbox_settings.enabled {
        match super::sandbox::check_available(sandbox_settings, sandbox_workspace) {
            Err(unavailable) => {
                return Err(std::io::Error::other(SandboxApply {
                    cause: unavailable.to_string(),
                }));
            }
            Ok(()) => {
                // The active shared hold crosses keeper death
                // (`docs/tools.md`, "The keeper"): the child inherits
                // its own copy, so a recovery probe sees WouldBlock
                // while the command lives. Hold-first: the take runs
                // before prepare reads the baseline, so a recovery that
                // won the race fails the spawn closed below instead of
                // running unprotected.
                let inherited = match baseline {
                    Some(path) => Some(super::sandbox::protected::inherit_active(path).map_err(
                        |error| {
                            std::io::Error::other(SandboxApply {
                                cause: error.to_string(),
                            })
                        },
                    )?),
                    None => None,
                };
                match super::sandbox::prepare(sandbox_settings, sandbox_workspace, baseline) {
                    Ok(prepared) => Some((prepared, inherited)),
                    Err(cause) => {
                        return Err(std::io::Error::other(SandboxApply {
                            cause: cause.to_string(),
                        }));
                    }
                }
            }
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
    if let Some((prepared, inherited)) = prepared {
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
        // syscalls: clearing CLOEXEC on the inherited active hold with
        // `fcntl`, then Landlock `restrict_self` and seccomp `apply_filter`
        // on a policy prepared before fork. Success performs no allocation:
        // the ruleset is consumed through the borrowed policy and the
        // seccomp program only borrowed, never moved or freed. A failure
        // writes the raw errno to the sentinel, without formatting or
        // allocation, and returns it, so spawn aborts and the command
        // never runs unconfined.
        // See `docs/code-quality.md`, "`unsafe`".
        unsafe {
            use std::os::unix::process::CommandExt as _;
            cmd.pre_exec(move || {
                if let Some(held) = inherited.as_ref()
                    && let Err(error) = clear_cloexec(held)
                {
                    let code = error.raw_os_error().unwrap_or(libc::EIO);
                    write_errno(&child, code);
                    return Err(std::io::Error::from_raw_os_error(code));
                }
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
            // Drop the parent-held pre_exec closure before waiting for the
            // sentinel. It owns the child stream; without closing that peer,
            // a failed write would leave the reader waiting forever.
            drop(cmd);
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

    Ok((child, listing, streams))
}

/// Runs one confined call on macOS: the shell spawns wrapped through
/// `sandbox-exec` with the prepared profile and bindings, its reader strips
/// the confinement marker, and the shared execution pipeline below hands off
/// only after the marker. The same helper serves production and the
/// rejected-profile test, which drives it with an intentionally invalid
/// profile: the wrapper then exits without a marker, nothing runs, and the
/// call fails closed with the wrapper's diagnostic. A spawn that never
/// starts fails the same way.
#[cfg(target_os = "macos")]
#[allow(
    clippy::too_many_arguments,
    reason = "spawn, the deadline, the move policy, the spill, the sandbox profile and the injected probe are one call"
)]
// mutants run on Linux, where this is not compiled.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn execute_confined(
    program: &Path,
    command: &str,
    workdir: &Path,
    timeout: Option<Duration>,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
    policy: MovePolicy,
    jobs: Option<&dyn Jobs>,
    spill: Option<SpillConfig>,
    matches: Option<&Path>,
    alive: fn(u32) -> bool,
    ps: &Path,
    profile: &str,
    bindings: &[(String, String)],
) -> Result<Ran, std::io::Error> {
    // The token is minted per call; the wrapper prints its marker before
    // the user program runs, so the marker's bytes precede any user byte
    // on the pipe and the exit status stays the command's.
    let token = super::sandbox::marker_token();
    let wrapped = super::sandbox::wrap_command(command, &token);
    let watch = Arc::new(super::sandbox::MarkerWatch::new(&token));
    let tty = policy == MovePolicy::Terminal;
    // The profile and every dynamic path travel as `-p` and `-D`
    // arguments: no path text is interpolated into the profile source.
    let mut cmd = Command::new(super::sandbox::SANDBOX_EXEC);
    cmd.arg("-p").arg(profile);
    for (name, value) in bindings {
        cmd.arg("-D").arg(format!("{name}={value}"));
    }
    cmd.arg("--")
        .arg(program)
        .arg("-c")
        .arg(&wrapped)
        .current_dir(workdir);
    let streams = open_streams(&mut cmd, policy)?;
    // The reader runs through the strip adapter fed by the call's marker
    // watch: exactly the first marker line is suppressed, and user output
    // stays byte-identical past it.
    let streams = SpawnStreams {
        input: streams.input,
        errors: streams.errors,
        read: Box::new(super::sandbox::StripMarker::new(
            streams.read,
            Arc::clone(&watch),
        )),
    };
    scrub_env(&mut cmd);
    if let Some(path) = matches {
        cmd.env(crate::search::matches::MATCHES_ENV, path);
    }
    support::group::detach(&mut cmd, tty);
    if !emit.starting() {
        return Ok(Ran::Declined);
    }
    let (child, listing) = match support::group::spawn(&mut cmd) {
        Ok(started) => started,
        Err(error) => {
            drop(cmd);
            return Err(std::io::Error::other(SandboxApply {
                cause: format!("sandbox-exec could not start: {error}"),
            }));
        }
    };
    // The parent drops every write end, or every secondary, so EOF arrives
    // when the last holder exits.
    drop(cmd);

    finish_spawned(
        super::hosted::Waiter::Child {
            child,
            listing,
            streams,
        },
        timeout,
        clock,
        cancel,
        emit,
        policy,
        jobs,
        spill,
        None,
        alive,
        ps,
        Drive::Gated(watch),
    )
}

/// Drives a macOS call, gating every transfer on the confinement marker:
/// no `MovePolicy` — Foreground's move-after threshold nor immediate
/// Background, Terminal or Monitor transfer — moves a confined call before
/// its marker is observed. Marker observed resumes normal move-policy
/// behavior; the post-drive check fails the unobserved remainder closed.
/// Unconfined calls drive exactly once, as before.
#[cfg(target_os = "macos")]
#[allow(
    clippy::too_many_arguments,
    reason = "the drive loop's inputs and the marker watch are one call"
)]
// mutants run on Linux, where this is not compiled.
#[cfg_attr(false, mutants::skip)]
fn drive_confined(
    progress: &mut Run,
    policy: MovePolicy,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
    alive: fn(u32) -> bool,
    ps: &Path,
    watch: Option<&super::sandbox::MarkerWatch>,
) -> LoopEnd {
    loop {
        match pump_inner(progress, policy, clock, cancel, emit, alive, ps) {
            LoopEnd::Finished(finished) => return LoopEnd::Finished(finished),
            LoopEnd::Move(reason) => {
                let gated = watch.is_some_and(|watch| !watch.observed());
                if !gated {
                    return LoopEnd::Move(reason);
                }
                // Park until the reader, the waiter, the clock or a cancel
                // wakes the loop: the move-after threshold cannot fire while
                // the gate holds, and the call's deadline still bounds the
                // wait. No new timer: the existing deadline applies.
                let seen = lock(&progress.shared.inner).seq;
                super::drive::park(
                    clock,
                    &progress.shared,
                    cancel,
                    progress.timeout_at,
                    true,
                    true,
                    seen,
                );
            }
        }
    }
}

/// Marks a socket as close-on-exec, so the sentinel never leaks into the
/// command. Runs in the parent before fork, where syscalls may fail with an
/// `io::Error`.
#[cfg(target_os = "linux")]
fn set_cloexec(stream: &std::os::unix::net::UnixStream) -> std::io::Result<()> {
    use rustix::fd::AsFd as _;
    let fd = stream.as_fd();
    let flags = rustix::io::fcntl_getfd(fd)?;
    rustix::io::fcntl_setfd(fd, flags.union(rustix::io::FdFlags::CLOEXEC))?;
    Ok(())
}

/// Clears close-on-exec on the inherited active hold, so the confined
/// command keeps it across exec while no unrelated spawn inherits it.
/// Runs between fork and exec, where only syscalls are sound: one `fcntl`
/// read and one `fcntl` write, preserving every other descriptor flag.
#[cfg(target_os = "linux")]
fn clear_cloexec(file: &std::fs::File) -> std::io::Result<()> {
    use rustix::fd::AsFd as _;
    let fd = file.as_fd();
    let flags = rustix::io::fcntl_getfd(fd)?;
    rustix::io::fcntl_setfd(fd, flags & !rustix::io::FdFlags::CLOEXEC)?;
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
    if parent.read_exact(&mut bytes).is_ok() {
        let code = i32::from_ne_bytes(bytes);
        Some(std::io::Error::other(SandboxApply {
            cause: std::io::Error::from_raw_os_error(code).to_string(),
        }))
    } else {
        None
    }
}

pub(super) fn wait_child(mut child: std::process::Child, shared: &Shared) {
    let status = child.wait().ok();
    let mut inner = lock(&shared.inner);
    inner.reaped = true;
    inner.status = status;
    bump(&mut inner);
    shared.cv.notify_all();
}

#[cfg(test)]
#[path = "keeper_lock_tests.rs"]
mod keeper_lock_tests;
#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
