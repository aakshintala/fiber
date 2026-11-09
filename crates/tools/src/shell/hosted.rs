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
use contract::events::JobCompleted;
use contract::jobs::Jobs;
use contract::keeper::{CommandHost, Exit, HostEvent, Sandbox, Spawn, Started};
use contract::tool::Cancel;

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
        env: matches
            .map(|path| {
                vec![(
                    crate::search::matches::MATCHES_ENV.into(),
                    path.to_string_lossy().into_owned(),
                )]
            })
            .unwrap_or_default(),
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
    let (sender, events) = std::sync::mpsc::channel();
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
// The sent_signal || to && mutant is equivalent: where the flags differ,
// assemble_output returns through Timeout or Capped before reading sent_signal.
#[cfg_attr(false, mutants::skip)]
pub fn kept_end(job_id: JobId, exit: &Exit) -> JobCompleted {
    let finished = Finished {
        output: Kept::Whole(Vec::new()),
        status: Some(status(exit)),
        stop: exit.timed_out.then_some(command::StopKind::Timeout),
        indeterminate: false,
        held_open: false,
        sent_signal: exit.timed_out || exit.output_cap,
        capped: exit.output_cap,
        flooded: false,
    };
    let mut completed = super::background::to_completed_reported(
        job_id,
        Path::new(""),
        finished,
        exit.timed_out.then_some(super::Limit {
            ms: 0,
            monitor: false,
        }),
        None,
    );
    completed.output_tail = None;
    completed
}

/// Starts a process using the same sandbox and stream builder as the local shell.
pub fn keeper_spawn(spec: &Spawn) -> io::Result<(Child, support::group::Listing, SpawnStreams)> {
    command::build(spec)
}

#[cfg(test)]
#[path = "hosted_tests.rs"]
mod tests;
