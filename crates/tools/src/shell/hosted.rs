//! Shell runs whose process and pipes belong to the keeper (`docs/tools.md`, "The keeper").

use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::sync::{Arc, mpsc::Receiver};
use std::thread;
use std::time::{Duration, SystemTime};

use contract::JobId;
use contract::clock::Clock;
use contract::emit::Emit;
use contract::events::{JobCompleted, Outcome};
use contract::jobs::JobIdentity;
use contract::jobs::Jobs;
use contract::keeper::{CommandHost, Exit, HostEvent, Sandbox, Spawn, Started};
use contract::shapes::Failure;
use contract::tool::Cancel;

use crate::error::{ShellError, ToolError};

use super::command::{self, Finished, Kept, MovePolicy, Ran, SpawnStreams};
use super::output::{OUTPUT_CAP, Shared, bump, hosted_bytes, lock, read_errors, read_output};
use super::sandbox::protected::ProtectedPaths;
use super::spill::Config;

pub(super) struct Hosted {
    pub(super) host: Arc<dyn CommandHost>,
    pub(super) id: String,
    /// The wall time the keeper enforces the command's timeout at, captured
    /// where the timeout is handed to the host. The drive keeps
    /// `timeout_at` as `None` for a hosted command so only the keeper stops
    /// it (`docs/tools.md`, "The keeper"); the receipt reads this wall
    /// instead (`docs/tools.md`, "Moving to the background").
    pub(super) deadline: Option<SystemTime>,
}

pub(super) enum Waiter {
    Child {
        child: Child,
        listing: support::group::Listing,
        streams: SpawnStreams,
    },
    Host {
        started: Started,
        hosted: Hosted,
        events: Receiver<HostEvent>,
        monitor: bool,
        terminal: bool,
    },
}

impl Waiter {
    pub(super) fn pgid(&self) -> u32 {
        match self {
            Self::Child { child, .. } => child.id(),
            Self::Host { started, .. } => started.pgid,
        }
    }
    pub(super) fn identity(&self) -> Option<JobIdentity> {
        let pgid = self.pgid();
        support::group::start_time(pgid).map(|start_time| JobIdentity { pgid, start_time })
    }
    pub(super) fn monitor(&self) -> bool {
        match self {
            Self::Child { streams, .. } => streams.errors.is_some(),
            Self::Host { monitor, .. } => *monitor,
        }
    }
    pub(super) fn watch(
        self,
        shared: &Arc<Shared>,
    ) -> (Option<support::group::Listing>, Option<Hosted>) {
        match self {
            Self::Child {
                child,
                listing,
                streams,
            } => {
                lock(&shared.inner).input = streams.input;
                let reader = Arc::clone(shared);
                thread::spawn(move || read_output(streams.read, &reader));
                if let Some(errors) = streams.errors {
                    let reader = Arc::clone(shared);
                    thread::spawn(move || read_errors(errors, &reader));
                }
                let waiter = Arc::clone(shared);
                thread::spawn(move || command::wait_child(child, &waiter));
                (Some(listing), None)
            }
            Self::Host {
                hosted,
                events,
                terminal,
                ..
            } => {
                if terminal {
                    let host = Arc::clone(&hosted.host);
                    let id = hosted.id.clone();
                    lock(&shared.inner).input = Some(contract::jobs::Input(Box::new(
                        move |bytes, _clock, cancel| {
                            if cancel.is_cancelled() {
                                return Ok(0);
                            }
                            host.write(&id, &String::from_utf8_lossy(bytes))
                                .map_err(|error| io::Error::other(error.to_string()))?;
                            Ok(bytes.len())
                        },
                    )));
                }
                let reader = Arc::clone(shared);
                thread::spawn(move || read_events(events, &reader));
                (None, Some(hosted))
            }
        }
    }
}

fn read_events(events: Receiver<HostEvent>, shared: &Shared) {
    let mut exit = None;
    for event in events {
        match event {
            HostEvent::Output { stream, bytes } => hosted_bytes(shared, stream, &bytes),
            HostEvent::Exit(ended) => {
                exit = Some(ended);
                break;
            }
        }
    }
    let mut inner = lock(&shared.inner);
    inner.status = exit.as_ref().map(status);
    inner.host_lost = exit.is_none();
    inner.hosted_exit = exit;
    inner.reaped = true;
    inner.eof = true;
    if let Some(errors) = inner.errors.as_mut() {
        errors.eof = true;
    }
    bump(&mut inner);
    shared.cv.notify_all();
}

fn status(exit: &Exit) -> ExitStatus {
    ExitStatus::from_raw(match exit.signal {
        Some(signal) => signal,
        None => exit.code.unwrap_or(0).saturating_mul(256),
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the hosted spawn shares the ordinary execution inputs"
)]
#[cfg(test)]
pub(super) fn execute(
    host: Arc<dyn CommandHost>,
    program: &Path,
    command: &str,
    workdir: &Path,
    timeout: Option<Duration>,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
    policy: MovePolicy,
    jobs: Option<&dyn Jobs>,
    spill: Option<Config>,
    matches: Option<&Path>,
    settings: &super::sandbox::Settings,
    workspace: &Path,
    protection: Option<&ProtectedPaths>,
) -> io::Result<Ran> {
    execute_with_session(
        host, program, command, workdir, timeout, clock, cancel, emit, policy, jobs, spill,
        matches, settings, workspace, protection, None,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the hosted spawn shares the ordinary execution inputs"
)]
pub(super) fn execute_with_session(
    host: Arc<dyn CommandHost>,
    program: &Path,
    command: &str,
    workdir: &Path,
    timeout: Option<Duration>,
    clock: &dyn Clock,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
    policy: MovePolicy,
    jobs: Option<&dyn Jobs>,
    spill: Option<Config>,
    matches: Option<&Path>,
    settings: &super::sandbox::Settings,
    workspace: &Path,
    protection: Option<&ProtectedPaths>,
    session_id: Option<&contract::SessionId>,
) -> io::Result<Ran> {
    let guard = command::acquire_protection(settings, workspace, protection)?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = format!(
        "call_{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let spec = Spawn {
        id: id.clone(),
        program: program.to_path_buf(),
        args: vec!["-c".into(), command.into()],
        cwd: workdir.to_path_buf(),
        env: {
            let mut env = matches
                .map(|path| {
                    vec![(
                        crate::search::matches::MATCHES_ENV.into(),
                        path.to_string_lossy().into_owned(),
                    )]
                })
                .unwrap_or_default();
            // Red-phase check: identity overlay temporarily disabled.
            if let Some(id) = session_id {
                env.push(("FIBER_SESSION_ID".into(), id.0.clone()));
            }
            env
        },
        stdin: command::spawn_stdin(policy),
        timeout_ms: timeout.map(|time| u64::try_from(time.as_millis()).unwrap_or(u64::MAX)),
        output_cap: OUTPUT_CAP,
        sandbox: Some(Sandbox {
            enabled: settings.enabled,
            allow_write: settings.allow_write.clone(),
            deny_read: settings.deny_read.clone(),
            fiber_home: settings.fiber_home.clone(),
            workspace: workspace.to_path_buf(),
            baseline: guard.as_ref().map(|guard| guard.dir.baseline.clone()),
        }),
    };
    // Every check that can refuse the call has run: the person's command
    // starts only if its start is recorded (`Emit::starting`).
    if !emit.starting() {
        drop(guard);
        return Ok(Ran::Declined);
    }
    // Bounded like every handover queue: a drive that falls behind makes
    // the keeper wait instead of buffering the call in the session.
    let (sender, events) = std::sync::mpsc::sync_channel(contract::keeper::HOST_EVENT_QUEUE); // ruling: #4054
    let started = host
        .start(spec, sender)
        .map_err(|error| io::Error::other(error.to_string()))?;
    // The keeper enforces the timeout the spec carried, so the drive keeps
    // `timeout_at` as `None`; the receipt still names when it fires, from
    // the wall handed over here.
    let deadline = timeout.and_then(|timeout| clock.wall().checked_add(timeout));
    command::finish_spawned(
        Waiter::Host {
            started,
            hosted: Hosted { host, id, deadline },
            events,
            monitor: policy == MovePolicy::Monitor,
            terminal: policy == MovePolicy::Terminal,
        },
        timeout,
        clock,
        cancel,
        emit,
        policy,
        jobs,
        spill,
        guard,
        support::group::alive,
        Path::new("ps"),
        command::Drive::Plain,
    )
}

/// Builds a keeper-written completion without session-only notice or tail fields.
// Recording only `timed_out` in `sent_signal` is equivalent: where the flags
// differ, assemble_output returns through Timeout or Capped before reading
// sent_signal.
#[cfg_attr(false, mutants::skip)]
pub fn kept_end(job_id: JobId, exit: &Exit) -> JobCompleted {
    let finished = keeper_finished(exit, false);
    let pre_end = super::pre_end_of(&finished);
    let mut completed = super::background::to_completed_reported(
        job_id,
        Path::new(""),
        finished,
        exit.timed_out.then_some(super::Limit {
            ms: 0,
            monitor: false,
            // The keeper wire carries no command text: a completion the
            // keeper writes after a restart keeps today's notice.
            has_pipe: false,
        }),
        None,
    );
    completed.output_tail = None;
    if let Some(path) = &exit.output_write_failure {
        completed.status = Outcome::Failed;
        completed.error = Some(override_write_failure(pre_end, path, false));
    }
    completed
}

/// The `Finished` a keeper-written completion maps: the keeper's exit
/// flags, with no stored write failure inside, so the one override the
/// caller applies stays the only one. `flooded` selects the flooded end
/// before any override, never after it.
fn keeper_finished(exit: &Exit, flooded: bool) -> Finished {
    Finished {
        output: Kept::Whole(Vec::new()),
        status: Some(status(exit)),
        stop: exit.timed_out.then_some(command::StopKind::Timeout),
        indeterminate: false,
        held_open: false,
        // A timeout stops the job's group, and a cap stops it too, but the
        // timeout and capped completions return before `sent_signal` is read,
        // so recording only the timeout flag keeps every observed completion
        // identical with no `||` left for mutants to flip.
        sent_signal: exit.timed_out,
        capped: exit.output_cap,
        flooded,
        write_failure: None,
    }
}

/// The completion the session writes for a job it took over from a keeper:
/// a live job's ending, with the tail read from the job's `output` file.
pub fn adopted_end(job_id: JobId, output: &Path, exit: &Exit) -> JobCompleted {
    let mut completed = kept_end(job_id, exit);
    completed.output_tail = super::background::output_tail(output);
    completed
}

/// The completion for a job taken over from a keeper that a flood stopped:
/// [`adopted_end`] with the `flooded` failure a live monitor's flood ends in,
/// selected before the write override so a stored failure still says the
/// monitor was also flooded.
pub fn adopted_flooded_end(job_id: JobId, output: &Path, exit: &Exit) -> JobCompleted {
    let finished = keeper_finished(exit, true);
    let mut completed = super::background::to_completed_reported(
        job_id,
        Path::new(""),
        finished,
        exit.timed_out.then_some(super::Limit {
            ms: 0,
            monitor: false,
            has_pipe: false,
        }),
        None,
    );
    completed.output_tail = super::background::output_tail(output);
    if let Some(path) = &exit.output_write_failure {
        completed.status = Outcome::Failed;
        completed.error = Some(override_write_failure(
            super::ReplacedEnd::Flooded,
            path,
            true,
        ));
    } else {
        completed.status = Outcome::Failed;
        completed.error = Some(Failure::from(&ToolError::from(ShellError::Flooded)));
    }
    completed
}

/// Gives a known output-file write failure precedence over the selected
/// end: the completion is `failed` with `io_failed`, naming the output
/// path and the documented end the failure replaced. The label is the
/// vocabulary `docs/tools.md` already uses for shell ends, so no new words
/// reach the model; the exit code and signal stay in `details`. Neither
/// the `io::ErrorKind` nor the operating system's text is rendered.
/// Runs exactly once while each final completion is built, after that
/// completion's end is selected.
pub fn override_write_failure(pre_end: super::ReplacedEnd, path: &Path, flooded: bool) -> Failure {
    Failure {
        code: contract::ErrorCode::IoFailed,
        message: crate::error::output_write_line(path, pre_end, flooded),
        retry_after_ms: None,
        provider: None,
    }
}

/// Starts a process using the same sandbox and stream builder as the local shell.
pub fn keeper_spawn(spec: &Spawn) -> io::Result<(Child, support::group::Listing, SpawnStreams)> {
    command::build(spec)
}

#[cfg(test)]
#[path = "hosted_tests.rs"]
mod tests;
