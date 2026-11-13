//! A detached shell command (`docs/tools.md`, "Background jobs"): the
//! command starts in its own session and process group, writes its output
//! to a log under Fiber home, and becomes a registry entry that only its
//! session can list and stop. One reaper thread per child owns the `Child`
//! and calls `wait`; a supervisor owns the registry's single `End`.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

use contract::clock::{Clock, Wake};
use contract::events::{JobCompleted, Outcome};
use contract::jobs::{JobIdentity, Opened};
use contract::shapes::{Failure, Process};
use rustix::process::Signal;
use support::clock::Parker;

use super::spawn::scrub_env;
use crate::error::ShellError;
use crate::error::ToolError;

/// How many log names one call tries before it fails: stale files from an
/// earlier process that reused the Fiber pid collide, and the person owns
/// deleting them, so 100 consecutive collisions fail the call instead of
/// scanning without bound.
const LOG_ATTEMPTS: u64 = 100;

/// The process-wide detached log sequence: one counter per process, so two
/// shells in one process never share a value. `create_log` takes its
/// counter as a parameter, so a test passes a test-local one and never
/// touches this static.
static NEXT: AtomicU64 = AtomicU64::new(0);

type Task = Box<dyn FnOnce() + Send + 'static>;

/// Thread creation for the supervisor; tests can force the fail-closed
/// path without changing the reaper's ownership.
pub(super) type Spawner = Arc<dyn Fn(Task) -> io::Result<()> + Send + Sync>;

/// The leader pid and identity, the log, and the completion channel for a
/// started detached command.
pub(super) struct Started {
    /// The child's process ID, which is also its process-group and session ID.
    pub pid: u32,
    /// The log the child's standard output and error go to.
    pub log: PathBuf,
    /// The identity recorded on its job row.
    pub identity: JobIdentity,
    /// The leader's wait result, sent by the sole reaper thread.
    done: Receiver<io::Result<ExitStatus>>,
    /// The stop request and wake signal shared with the registry's Stop.
    pub control: Arc<StopControl>,
}

/// The stop callback signals TERM synchronously; the supervisor owns the
/// grace period, escalation and `End` report.
pub(super) struct StopControl {
    stop: Mutex<StopState>,
    parker: Arc<Parker>,
}

#[derive(Default)]
struct StopState {
    requested: bool,
    result: Option<io::Result<()>>,
}

impl StopControl {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            stop: Mutex::new(StopState::default()),
            parker: Arc::new(Parker::new()),
        })
    }

    // The supervisor's poll cap observes clock changes without subscription; this avoids only a bounded poll delay.
    #[cfg_attr(false, mutants::skip)]
    fn subscribe(&self, clock: &dyn Clock) {
        let wake: Arc<dyn Wake> = self.parker.clone();
        clock.subscribe(Arc::downgrade(&wake));
    }

    /// Reached only after the registry re-read the leader identity and
    /// approved the stop. The signal is immediate, before the supervisor's
    /// clock-based grace period.
    pub(super) fn request_stop(&self, pgid: u32) {
        let mut state = support::lock(&self.stop);
        if state.requested {
            return;
        }
        state.requested = true;
        state.result = Some(support::group::signal(pgid, Signal::TERM).map_err(io::Error::from));
        drop(state);
        self.parker.bump();
    }

    fn take_stop(&self) -> Option<io::Result<()>> {
        support::lock(&self.stop).result.take()
    }

    // The supervisor's poll cap observes reaper completion without this wake; this avoids only a bounded poll delay.
    #[cfg_attr(false, mutants::skip)]
    fn wake(&self) {
        self.parker.bump();
    }
}

/// Starts `command` detached: its own session and group, outside the
/// sandbox, writing to a fresh log in `dir`. Standard input is `/dev/null`;
/// standard output and standard error are the log file and its duplicate,
/// one shared offset. The leader identity is read before it is admitted to
/// the registry. If a reaper thread cannot start, this call waits for the
/// child itself, then returns the same completion channel so the sole child handle is never dropped without being waited.
pub(super) fn launch(
    dir: &Path,
    fiber_pid: u32,
    program: &Path,
    command: &str,
    workdir: &Path,
) -> Result<Started, io::Error> {
    // The file name is always UTF-8, so only the directory can refuse
    // this: a log with no path the model can read fails before spawn, so
    // no process starts without a path for the receipt.
    if dir.to_str().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("detached log directory is not UTF-8: {}", dir.display()),
        ));
    }
    let (log, path) = create_log(dir, fiber_pid, &NEXT)?;
    let mut cmd = Command::new(program);
    cmd.arg("-c")
        .arg(command)
        .current_dir(workdir)
        .stdin(Stdio::null());
    let err = log.try_clone()?;
    cmd.stdout(log).stderr(err);
    scrub_env(&mut cmd);
    support::group::detach(&mut cmd, false);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(source) => {
            let _removed = std::fs::remove_file(&path);
            return Err(source);
        }
    };
    let pid = child.id();
    let Some(start_time) = support::group::start_time(pid) else {
        let _killed = support::group::signal(pid, Signal::KILL);
        let mut child = child;
        let _reaped = child.wait();
        let _removed = std::fs::remove_file(&path);
        return Err(io::Error::other(format!(
            "could not read the detached leader identity for process {pid}"
        )));
    };
    let control = StopControl::new();
    let (tx, done) = mpsc::channel();
    reap(child, tx, Arc::clone(&control));
    Ok(Started {
        pid,
        log: path,
        identity: JobIdentity {
            pgid: pid,
            start_time,
        },
        done,
        control,
    })
}

/// Creates the detached log directory and the next free log in it: each
/// attempt takes one sequence value and opens `d_<fiber-pid>_<n>.log` with
/// `create_new`, so two concurrent calls never share a file. The directory
/// is mode 0700 and each file mode 0600; a directory that already exists
/// keeps its mode.
fn create_log(dir: &Path, fiber_pid: u32, next: &AtomicU64) -> io::Result<(File, PathBuf)> {
    DirBuilder::new().mode(0o700).recursive(true).create(dir)?;
    for _ in 0..LOG_ATTEMPTS {
        let n = next.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("d_{fiber_pid}_{n}.log"));
        match OpenOptions::new()
            .create_new(true)
            .append(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(taken) if taken.kind() == io::ErrorKind::AlreadyExists => {}
            Err(failed) => return Err(failed),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "{LOG_ATTEMPTS} detached logs named d_{fiber_pid}_* already exist in {}",
            dir.display()
        ),
    ))
}

/// Creates the production thread spawner for detached supervisors.
pub(super) fn system_spawner() -> Spawner {
    Arc::new(|task| {
        std::thread::Builder::new()
            .name("detached-supervisor".to_owned())
            .spawn(task)
            .map(drop)
    })
}

/// Moves a detached supervisor to its thread. If that thread cannot start,
/// the caller keeps the `End`, kills the group and reports one failure
/// synchronously rather than leaving a running row that has no supervisor to stop it.
pub(super) struct Restored {
    pub pgid: u32,
    pub control: Arc<StopControl>,
    pub log_root: PathBuf,
}

/// Starts a childless supervisor for a process group admitted from the
/// session log. Failure reports one indeterminate End without signalling.
pub(super) fn track_restored(
    opened: Opened,
    restored: Restored,
    clock: Arc<dyn Clock>,
    spawner: &Spawner,
) -> Result<(), io::Error> {
    restored.control.subscribe(clock.as_ref());
    let supervisor = Arc::new(Mutex::new(Some(RestoredSupervisor {
        opened,
        restored,
        clock,
    })));
    let thread_supervisor = Arc::clone(&supervisor);
    let task: Task = Box::new(move || {
        if let Some(supervisor) = support::lock(&thread_supervisor).take() {
            supervise_restored(supervisor);
        }
    });
    match spawner(task) {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Some(supervisor) = support::lock(&supervisor).take() {
                supervisor.fail_indeterminate();
            }
            Err(error)
        }
    }
}

pub(super) fn track(
    opened: Opened,
    started: Started,
    clock: Arc<dyn Clock>,
    spawner: &Spawner,
) -> Result<(), io::Error> {
    started.control.subscribe(clock.as_ref());
    let supervisor = Arc::new(Mutex::new(Some(Supervisor {
        opened,
        started,
        clock,
    })));
    let thread_supervisor = Arc::clone(&supervisor);
    let task: Task = Box::new(move || {
        if let Some(supervisor) = support::lock(&thread_supervisor).take() {
            supervise(supervisor);
        }
    });
    match spawner(task) {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Some(supervisor) = support::lock(&supervisor).take() {
                supervisor.fail_to_start(&error);
            }
            Err(error)
        }
    }
}

struct RestoredSupervisor {
    opened: Opened,
    restored: Restored,
    clock: Arc<dyn Clock>,
}

impl RestoredSupervisor {
    fn report(self, status: Outcome, error: Option<ShellError>) {
        let completed = JobCompleted {
            job_id: self.opened.started.job_id.clone(),
            status,
            error: error.map(failure),
            process: None,
            output_tail: super::background::output_tail_from(
                &self.opened.path,
                Some(&self.restored.log_root),
            ),
            notice: None,
            changed_by: None,
            protected_paths: None,
        };
        (self.opened.end.0)(completed);
    }

    fn fail_indeterminate(self) {
        self.report(Outcome::Failed, Some(ShellError::Indeterminate));
    }

    fn fail_signal(self, source: io::Error) {
        self.report(Outcome::Failed, Some(ShellError::NotStarted { source }));
    }
}

/// Watches a restored group without a child handle. A spontaneous
/// disappearance has no observable exit status, even after an approved stop.
fn supervise_restored(supervisor: RestoredSupervisor) {
    let pgid = supervisor.restored.pgid;
    let control = Arc::clone(&supervisor.restored.control);
    let mut cancelled = false;
    let mut term_deadline = None;
    let mut drain_deadline = None;
    loop {
        let seen = control.parker.generation();
        if let Some(result) = control.take_stop() {
            match result {
                Ok(()) => {
                    cancelled = true;
                    let now = supervisor.clock.now();
                    term_deadline = Some(now.checked_add(support::group::GRACE).unwrap_or(now));
                }
                Err(source) => {
                    supervisor.fail_signal(source);
                    return;
                }
            }
        }
        if !support::group::alive(pgid) {
            if cancelled {
                supervisor.report(Outcome::Failed, Some(ShellError::Indeterminate));
            } else {
                supervisor.fail_indeterminate();
            }
            return;
        }
        let now = supervisor.clock.now();
        if term_deadline.is_some_and(|deadline| now >= deadline) {
            term_deadline = None;
            if let Err(error) = support::group::signal(pgid, Signal::KILL) {
                supervisor.fail_signal(io::Error::from(error));
                return;
            }
            drain_deadline = Some(now.checked_add(support::group::DRAIN).unwrap_or(now));
        }
        if drain_deadline.is_some_and(|deadline| now >= deadline && support::group::alive(pgid)) {
            supervisor.fail_indeterminate();
            return;
        }
        control.parker.park_polled(
            supervisor.clock.as_ref(),
            term_deadline.or(drain_deadline),
            Some(support::group::GROUP_POLL),
            seen,
        );
    }
}

struct Supervisor {
    opened: Opened,
    started: Started,
    clock: Arc<dyn Clock>,
}

impl Supervisor {
    fn finish(self, result: io::Result<ExitStatus>, cancelled: bool) {
        let Opened {
            started, path, end, ..
        } = self.opened;
        let completed = completed(started.job_id, &path, result, cancelled);
        (end.0)(completed);
    }

    fn fail_to_start(self, error: &io::Error) {
        let _killed = support::group::signal(self.started.pid, Signal::KILL);
        let Opened {
            started, path, end, ..
        } = self.opened;
        (end.0)(failed(
            started.job_id,
            &path,
            ShellError::NotStarted {
                source: io::Error::other(error.to_string()),
            },
        ));
    }

    fn fail_indeterminate(self) {
        let Opened {
            started, path, end, ..
        } = self.opened;
        (end.0)(failed(started.job_id, &path, ShellError::Indeterminate));
    }

    fn fail_signal(self, source: io::Error) {
        let Opened {
            started, path, end, ..
        } = self.opened;
        (end.0)(failed(
            started.job_id,
            &path,
            ShellError::NotStarted { source },
        ));
    }
}

/// Owns the single `End`: watches the leader's reaper, the process group,
/// and the stop callback. A stop sends TERM synchronously in that callback;
/// this thread waits GRACE, escalates to KILL, and drains for DRAIN.
fn supervise(supervisor: Supervisor) {
    let pgid = supervisor.started.pid;
    let control = Arc::clone(&supervisor.started.control);
    let mut reaped: Option<io::Result<ExitStatus>> = None;
    let mut cancelled = false;
    let mut term_deadline = None;
    let mut drain_deadline = None;
    loop {
        let seen = control.parker.generation();
        if let Some(result) = control.take_stop() {
            match result {
                Ok(()) => {
                    cancelled = true;
                    let now = supervisor.clock.now();
                    term_deadline = Some(now.checked_add(support::group::GRACE).unwrap_or(now));
                }
                Err(source) => {
                    supervisor.fail_signal(source);
                    return;
                }
            }
        }
        if reaped.is_none() {
            reaped = match supervisor.started.done.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(io::Error::other(
                    "the detached reaper ended without a wait result",
                ))),
            };
        }
        if reaped.is_some() && !support::group::alive(pgid) {
            if let Some(result) = reaped.take() {
                supervisor.finish(result, cancelled);
            }
            return;
        }

        let now = supervisor.clock.now();
        if term_deadline.is_some_and(|deadline| now >= deadline) {
            term_deadline = None;
            if support::group::alive(pgid) {
                if let Err(error) = support::group::signal(pgid, Signal::KILL) {
                    supervisor.fail_signal(io::Error::from(error));
                    return;
                }
                drain_deadline = Some(now.checked_add(support::group::DRAIN).unwrap_or(now));
            }
        }
        if drain_deadline.is_some_and(|deadline| now >= deadline && support::group::alive(pgid)) {
            supervisor.fail_indeterminate();
            return;
        }
        supervisor.started.control.parker.park_polled(
            supervisor.clock.as_ref(),
            term_deadline.or(drain_deadline),
            Some(support::group::GROUP_POLL),
            seen,
        );
    }
}

fn completed(
    job_id: contract::JobId,
    path: &Path,
    result: io::Result<ExitStatus>,
    cancelled: bool,
) -> JobCompleted {
    match result {
        Ok(status) => {
            let (exit_code, signal, _) = super::observed(status);
            let process = Process {
                exit_code,
                signal: signal.clone(),
                timed_out: false,
            };
            let (status, error) = if cancelled {
                (Outcome::Cancelled, None)
            } else if exit_code == Some(0) {
                (Outcome::Completed, None)
            } else if let Some(code) = exit_code {
                (Outcome::Failed, Some(failure(ShellError::Exited { code })))
            } else if let Some(signal) = signal {
                (
                    Outcome::Failed,
                    Some(failure(ShellError::Killed { signal })),
                )
            } else {
                (Outcome::Failed, Some(failure(ShellError::NotReaped)))
            };
            JobCompleted {
                job_id,
                status,
                error,
                process: Some(process),
                output_tail: super::background::output_tail(path),
                notice: None,
                changed_by: None,
                protected_paths: None,
            }
        }
        Err(_) => failed(job_id, path, ShellError::NotReaped),
    }
}

fn failed(job_id: contract::JobId, path: &Path, error: ShellError) -> JobCompleted {
    JobCompleted {
        job_id,
        status: Outcome::Failed,
        error: Some(failure(error)),
        process: None,
        output_tail: super::background::output_tail(path),
        notice: None,
        changed_by: None,
        protected_paths: None,
    }
}

fn failure(error: ShellError) -> Failure {
    let error: ToolError = error.into();
    Failure::from(&error)
}

/// Kills the just-launched group after registry admission fails. The
/// independent reaper keeps ownership of the child and will still wait it.
pub(super) fn abort(started: Started) {
    let _killed = support::group::signal(started.pid, Signal::KILL);
}

/// The call's receipt names the session job and the one supported stop path.
pub(super) fn receipt(log: &Path, job_id: &str) -> String {
    format!(
        "Started detached. Job {job_id}. Output: {}. Stop it with `jobs stop {job_id}`.\n",
        log.display()
    )
}

fn reap(child: Child, tx: Sender<io::Result<ExitStatus>>, control: Arc<StopControl>) {
    let child = Arc::new(Mutex::new(Some(child)));
    let thread_child = Arc::clone(&child);
    let thread_tx = tx.clone();
    let thread_control = Arc::clone(&control);
    let spawned = std::thread::Builder::new()
        .name("detached-reaper".to_owned())
        .spawn(move || {
            let result = match support::lock(&thread_child).take() {
                Some(mut child) => child.wait(),
                None => Err(io::Error::other("the detached child was already taken")),
            };
            let _sent = thread_tx.send(result);
            thread_control.wake();
        });
    if spawned.is_err() {
        // The child handle must have exactly one waiter. If the reaper
        // thread cannot start, the launching thread waits and delivers the
        // result through the same channel.
        let result = match support::lock(&child).take() {
            Some(mut child) => child.wait(),
            None => Err(io::Error::other("the detached child was already taken")),
        };
        let _sent = tx.send(result);
        control.wake();
    }
}

#[cfg(test)]
#[path = "detached_tests.rs"]
mod tests;
