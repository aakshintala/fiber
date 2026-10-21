//! The session's jobs: one id, one output file, one lifecycle
//! (`docs/tools.md`, "Background jobs").

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use contract::JobId;
use contract::clock::{Clock, Wake};
use contract::diag::Timing;
use contract::emit::Emit;
use contract::events::{
    DelegateFinished, DelegateStarted, JobCompleted, JobLine, JobStarted, Outcome,
};
use contract::inbox::Delivery;
use contract::jobs::{
    End, Foreground, Lines, OpenError, Opened, Opening, Stop, StopError, WriteError,
};
use contract::tool::Cancel;

#[path = "registry/park.rs"]
mod park;

pub(crate) mod runs;

use park::{Parked, park_until};
use runs::{Answer, Pending, RunEnd};
use support::clock::Parker;

/// A job's end not yet reported, held by the closure in its [`End`] or,
/// for a delegate, by the runner until it reports. Reporting consumes
/// it; dropped unreported, it records the job failed `indeterminate`,
/// since a runner that returned without reporting would otherwise leave
/// the job running. A panic aborts the process; this is not a panic
/// handler.
pub(crate) struct Unreported {
    job_id: JobId,
    /// The run this report belongs to: it records nothing on a later one.
    run: u64,
    /// Taken by the one report.
    registry: Option<Arc<Registry>>,
    /// Whether this is a delegate: its drop carries an empty finish.
    delegate: bool,
}

impl Unreported {
    /// Records `completed` for this job, whatever id the payload names: a
    /// payload that names another id would record the wrong job, or none.
    /// The payload's id is replaced with the recorded one. For a delegate
    /// the runner passes its finish alongside, when the run produced one.
    pub(crate) fn report(mut self, completed: JobCompleted, delegate: Option<DelegateFinished>) {
        if let Some(registry) = self.registry.take() {
            registry.finish_run(
                self.run,
                JobCompleted {
                    job_id: self.job_id.clone(),
                    ..completed
                },
                delegate,
            );
        }
    }
}

impl Drop for Unreported {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.take() {
            registry.fail_job(self.job_id.clone(), self.run, self.delegate);
        }
    }
}

/// The jobs one session started. `open` is the only way in; `list`, `wait`
/// and `stop` are what the `jobs` tool calls.
pub struct Registry {
    artifacts: PathBuf,
    clock: Arc<dyn Clock>,
    /// Handed to each opened job for its `job_delta` lines.
    emit: Arc<dyn Emit>,
    inner: Mutex<Inner>,
    /// What `wait` and `stop` block on: a job's end, a cancel and a clock
    /// move all bump it.
    park: Parker,
    /// Held by `admit` and `resume` from a delegate start's cap check
    /// through its commit. Taken before the inner lock; nothing takes
    /// them the other way.
    start_gate: Mutex<()>,
    /// Times a background start as `jobs.start`, installed once by the
    /// session before any tool runs. The first install wins; an empty
    /// cell times nothing.
    timing: OnceLock<Timing>,
    /// Upgrades to the `Arc` `new` returned, so `open` can hand that `Arc`
    /// to the job's [`End`] while taking `&self`.
    me: Weak<Self>,
}

struct Inner {
    jobs: Vec<Job>,
    /// The loop's inbox, which a job's end is sent to. `None` until
    /// [`contract::jobs::Jobs::deliver_to`].
    inbox: Option<Sender<Delivery>>,
    /// The watcher-side budget update the loop installed, if any. Held
    /// under this lock, cloned out before it runs, so the tap never runs
    /// with the lock held.
    usage_tap: Option<contract::jobs::UsageTap>,
    /// Running foreground calls, held weakly (`Jobs::foreground`).
    foreground: Vec<Weak<dyn Fn() -> bool + Send + Sync>>,
    /// The loop set this on an end path, before that path's first
    /// `running()` read: every later extension-owned delegate start is
    /// refused with `closing`. Set once, never cleared.
    extension_starts_closed: bool,
    /// A stop-all has been issued (`stop_delegates`): every delegate
    /// commit after it stops at once, so a stop racing a gated start
    /// cannot miss the starting delegate. Set under this lock, never
    /// cleared: the one-way trip stays one-way.
    stop_all_delegates: bool,
}

struct Job {
    started: JobStarted,
    path: PathBuf,
    phase: Phase,
    stop: Arc<dyn Fn() + Send + Sync>,
    /// Types into the job's terminal; `None` unless it started with `tty`.
    input: Option<Typer>,
    stop_sent: bool,
    /// The delegate's first run, when this is one: only `delegate_spawn`
    /// or `delegate_fork` opens one. `stop_delegates` reads it.
    delegate: Option<DelegateStarted>,
    /// A delegate's finish, kept past the end for a later `wait`.
    finished: Option<DelegateFinished>,
    /// The running run's number, never reused.
    run: u64,
    /// The run's unwritten records, in the order they happened.
    queue: VecDeque<Pending>,
    /// The final state of every run a resume superseded, never removed.
    ends: Vec<RunEnd>,
}

type Typer = Arc<dyn Fn(&[u8], &dyn Clock, &dyn Cancel) -> std::io::Result<usize> + Send + Sync>;

/// A run's stop with its run's number: the closure to send, when this
/// call sends it, and the run it stops.
type RunStop = (Option<Arc<dyn Fn() + Send + Sync>>, u64);

impl Job {
    /// A job that has started and not yet ended; `open` and `open_started`
    /// both record one, and only differ in what they pass in.
    fn running(
        started: JobStarted,
        path: PathBuf,
        stop: Arc<dyn Fn() + Send + Sync>,
        input: Option<Typer>,
        delegate: Option<DelegateStarted>,
    ) -> Self {
        Self {
            started,
            path,
            phase: Phase::Running,
            stop,
            input,
            stop_sent: false,
            delegate,
            finished: None,
            run: 1,
            queue: VecDeque::new(),
            ends: Vec::new(),
        }
    }
}

enum Phase {
    Running,
    /// Boxed: `JobCompleted` is far larger than `Running`, and a registry
    /// holds one phase per job.
    Ended(Box<JobCompleted>),
}

impl Registry {
    /// Jobs whose output files are created in `artifacts`. `clock` is the
    /// session clock: a `wait` deadline is read from it. `emit` carries the
    /// jobs' `job_delta` lines.
    pub fn new(artifacts: PathBuf, clock: Arc<dyn Clock>, emit: Arc<dyn Emit>) -> Arc<Self> {
        let registry = Arc::new_cyclic(|me| Self {
            artifacts,
            clock: Arc::clone(&clock),
            emit,
            inner: Mutex::new(Inner {
                jobs: Vec::new(),
                inbox: None,
                usage_tap: None,
                foreground: Vec::new(),
                extension_starts_closed: false,
                stop_all_delegates: false,
            }),
            park: Parker::new(),
            start_gate: Mutex::new(()),
            timing: OnceLock::new(),
            me: Weak::clone(me),
        });
        let wake: Arc<dyn Wake> = registry.clone();
        clock.subscribe(Arc::downgrade(&wake));
        registry
    }

    /// Installs the timing a background start is timed as `jobs.start`
    /// with. The first install wins. Both production callers gate on the
    /// existing debug check, so at info the cell stays empty and a start
    /// costs one `OnceLock::get`.
    pub fn install_timing(&self, timing: Timing) {
        // A second install is ignored: the first wins.
        self.timing.get_or_init(|| timing);
    }

    /// The installed timing, if any.
    pub(crate) fn timing(&self) -> Option<&Timing> {
        self.timing.get()
    }

    /// Mints the id, creates the empty output file, and records the job
    /// running. Does not create `artifacts`: when the file cannot be
    /// created, returns [`OpenError`] and records nothing.
    pub fn open(&self, opening: Opening) -> Result<Opened, OpenError> {
        let Some(registry) = self.me.upgrade() else {
            return Err(OpenError {
                path: self.artifacts.clone(),
                source: std::io::Error::other("the registry is gone"),
            });
        };
        let mut inner = lock(&self.inner);
        let id = mint_id();
        let path = self.artifacts.join(format!("{id}.log"));
        let file = match File::create(&path) {
            Ok(file) => file,
            Err(source) => return Err(OpenError { path, source }),
        };
        let job_id = JobId(id.clone());
        let started = JobStarted {
            job_id: job_id.clone(),
            tool: Some(opening.tool),
            extension: None,
            description: opening.description,
            output_path: format!("artifacts/{id}.log"),
        };
        let lines = opening.lines.then(|| {
            let registry = Weak::clone(&self.me);
            Lines(Box::new(move |line| {
                if let Some(registry) = registry.upgrade() {
                    registry.send_line(line);
                }
            }))
        });
        let unreported = Unreported {
            job_id,
            run: 1,
            registry: Some(registry),
            delegate: false,
        };
        let end = End(Box::new(move |completed| {
            unreported.report(completed, None)
        }));
        inner.jobs.push(Job::running(
            started.clone(),
            path.clone(),
            Arc::from(opening.stop.0),
            opening.input.map(|input| Arc::from(input.0)),
            None,
        ));
        Ok(Opened {
            started,
            path,
            file,
            end,
            emit: Arc::clone(&self.emit),
            lines,
        })
    }

    /// Records a running Fiber delegate under `job_id`, without creating
    /// an output file: the child writes its own log at `output_path`.
    /// Only `delegate_spawn` or `delegate_fork` opens one. The id is minted up front with
    /// [`mint_job_id`], so the spawn and the record name the same job.
    pub(crate) fn open_started(
        self: &Arc<Self>,
        job_id: JobId,
        tool: String,
        description: String,
        output_path: String,
        stop: Stop,
        delegate: DelegateStarted,
    ) -> (JobStarted, Unreported) {
        let started = JobStarted {
            job_id: job_id.clone(),
            tool: Some(tool),
            extension: None,
            description,
            output_path: output_path.clone(),
        };
        let finish = Unreported {
            job_id: job_id.clone(),
            run: 1,
            // The caller holds this `Arc`, so the job it records always
            // has a registry to end in.
            registry: Some(Arc::clone(self)),
            delegate: true,
        };
        let mut inner = lock(&self.inner);
        inner.jobs.push(Job::running(
            started.clone(),
            PathBuf::from(output_path),
            Arc::from(stop.0),
            None,
            Some(delegate),
        ));
        // A stop-all racing this start latches under this same lock
        // before this push, or this push lands before its scan: either
        // way the starting delegate stops, instead of escaping the
        // one-way trip unregistered. The stop runs with the lock
        // released; the admission gate stays held by the caller, as when
        // `retain_extensions` sends its stops.
        let latched = if inner.stop_all_delegates {
            inner.jobs.last_mut().map(|job| {
                job.stop_sent = true;
                Arc::clone(&job.stop)
            })
        } else {
            None
        };
        drop(inner);
        if let Some(stop) = latched {
            stop();
        }
        (started, finish)
    }

    /// Whether a session has wired its inbox for delegate starts and ends.
    pub(crate) fn has_inbox(&self) -> bool {
        lock(&self.inner).inbox.is_some()
    }

    /// The forwarding callback the delegate runner taps each well-formed
    /// `usage_recorded` envelope with: the loop's budget update first, on
    /// the watcher thread, then the inbox offer. Cloned taps and senders
    /// run after the lock is released; a registry without a tap or inbox
    /// keeps the existing unwired behavior. Never retains a strong
    /// registry reference in the callback the registry stores.
    pub(crate) fn usage_forward(self: &Arc<Self>) -> super::delegate::run::UsageForward {
        let weak = Arc::downgrade(self);
        Arc::new(move |session, line| {
            let Some(registry) = weak.upgrade() else {
                return;
            };
            let (tap, inbox) = {
                let inner = lock(&registry.inner);
                (inner.usage_tap.clone(), inner.inbox.clone())
            };
            if let Some(tap) = tap {
                tap(line.clone());
            }
            if let Some(inbox) = inbox {
                let _sent = inbox.send(Delivery::DelegateUsage {
                    session: session.clone(),
                    line,
                });
            }
        })
    }

    /// Sends the stop of every running job whose `JobStarted.extension` is
    /// `Some(name)` with `name` not in `loaded`, once per run, and returns
    /// how many were sent one. Takes `start_gate` before `inner`, the
    /// order `admit` takes them, so a start is either recorded before this
    /// reads the jobs or not yet admitted. Calls the stops with `inner`
    /// released, and releases `start_gate` last. Model jobs, shell jobs,
    /// ended jobs and jobs already sent a stop are left alone.
    pub fn retain_extensions(&self, loaded: &[String]) -> usize {
        let _gate = lock(&self.start_gate);
        let stops: Vec<Arc<dyn Fn() + Send + Sync>> = {
            let mut inner = lock(&self.inner);
            inner
                .jobs
                .iter_mut()
                .filter(
                    |job| matches!(&job.started.extension, Some(name) if !loaded.contains(name)),
                )
                .filter(|job| !job.stop_sent)
                .filter(|job| matches!(job.phase, Phase::Running))
                .map(|job| {
                    job.stop_sent = true;
                    Arc::clone(&job.stop)
                })
                .collect()
        };
        for stop in &stops {
            stop();
        }
        stops.len()
    }

    /// Records an extension-owned delegate and sends its start under the
    /// same lock, before a reader can see it in the running list.
    pub(crate) fn open_extension_started(
        self: &Arc<Self>,
        job_id: JobId,
        extension: String,
        description: String,
        output_path: String,
        stop: Stop,
        delegate: DelegateStarted,
    ) -> Unreported {
        let started = JobStarted {
            job_id: job_id.clone(),
            tool: None,
            extension: Some(extension),
            description,
            output_path: output_path.clone(),
        };
        let finish = Unreported {
            job_id,
            run: 1,
            registry: Some(Arc::clone(self)),
            delegate: true,
        };
        let mut inner = lock(&self.inner);
        // Send before registering, under this same lock: a running() reader
        // that sees the job can already take its start in the second drain.
        if let Some(inbox) = &inner.inbox {
            let _sent = inbox.send(Delivery::ExtensionJob(started.clone(), delegate.clone()));
        }
        inner.jobs.push(Job::running(
            started,
            PathBuf::from(output_path),
            Arc::from(stop.0),
            None,
            Some(delegate),
        ));
        // As in `open_started`: a latched stop-all stops the starting
        // delegate at once, with the lock released.
        let latched = if inner.stop_all_delegates {
            inner.jobs.last_mut().map(|job| {
                job.stop_sent = true;
                Arc::clone(&job.stop)
            })
        } else {
            None
        };
        drop(inner);
        if let Some(stop) = latched {
            stop();
        }
        finish
    }

    /// Sends a monitor's batch to the inbox, under the lock an end sends
    /// under, so the job's lines and its end arrive in the order its drive
    /// thread made them. Dropped when no inbox is set.
    fn send_line(&self, line: JobLine) {
        let inner = lock(&self.inner);
        if let Some(inbox) = &inner.inbox {
            // A loop that is gone takes no news.
            let _sent = inbox.send(Delivery::JobLine(line));
        }
    }

    /// The list the model sees, in start order. One line per job, or
    /// `No jobs.` when the session has none.
    pub(crate) fn list_text(&self) -> String {
        let inner = lock(&self.inner);
        if inner.jobs.is_empty() {
            return "No jobs.\n".to_owned();
        }
        let mut text = String::new();
        for job in &inner.jobs {
            text.push_str(&entry_text(job));
        }
        text
    }

    /// Whether `id` is a job this registry opened.
    pub(crate) fn contains(&self, id: &str) -> bool {
        lock(&self.inner)
            .jobs
            .iter()
            .any(|job| job.started.job_id.0 == id)
    }

    /// Blocks until `id`'s current run ends, `timeout_ms` passes, or
    /// `cancel` fires. Each call reads the run once and answers that
    /// run's final state, whatever run is current by then. A job that has
    /// already ended returns at once. A timeout or a cancel leaves the
    /// job running.
    pub(crate) fn wait(
        self: &Arc<Self>,
        id: &str,
        timeout_ms: u64,
        cancel: &dyn Cancel,
    ) -> Result<Answer, ()> {
        let Some(run) = self.run_of(id) else {
            return Err(());
        };
        if let Some(answer) = self.answer_for(id, run) {
            return Ok(answer);
        }
        // `checked_add` is `None` when the timeout does not fit on the
        // clock. That wait has no deadline: it runs until the job ends or
        // the wait is cancelled.
        let until = self
            .clock
            .now()
            .checked_add(Duration::from_millis(timeout_ms));
        match self.park_until(id, run, until, cancel) {
            Parked::Ended => Ok(self
                .answer_for(id, run)
                .unwrap_or_else(|| self.still_running(id))),
            Parked::Timeout | Parked::Cancelled => Ok(self.still_running(id)),
        }
    }

    /// Asks `id`'s current run to stop, once per run, then waits until it
    /// ends or `cancel` fires. A cancel returns at once; the stop already
    /// sent stands.
    pub(crate) fn stop(
        self: &Arc<Self>,
        id: &str,
        cancel: &dyn Cancel,
    ) -> Result<Answer, StopError> {
        let (stop, run) = self.send_stop(id)?;
        if let Some(stop) = stop {
            stop();
        }
        if let Some(answer) = self.answer_for(id, run) {
            return Ok(answer);
        }
        match self.park_until(id, run, None, cancel) {
            Parked::Ended => Ok(self
                .answer_for(id, run)
                .unwrap_or_else(|| self.still_running(id))),
            Parked::Timeout | Parked::Cancelled => Ok(self.still_running(id)),
        }
    }

    /// Types `input` into `id`'s terminal, then returns the output that
    /// arrives until `wait_ms` passes on the clock, the job ends, or
    /// `cancel` fires. The output is the file's bytes from its length
    /// before the write. An ended job's final state follows the output and
    /// is claimed as `wait` claims it.
    pub(crate) fn write(
        self: &Arc<Self>,
        id: &str,
        input: &str,
        wait_ms: u64,
        cancel: &dyn Cancel,
    ) -> Result<Answer, WriteError> {
        let (typer, path) = self.typer_of(id)?;
        // Read before the write, so output the write causes is after it.
        let from = std::fs::metadata(&path).map_or(0, |meta| meta.len());
        let written =
            typer(input.as_bytes(), self.clock.as_ref(), cancel).map_err(WriteError::Io)?;
        let until = self.clock.now().checked_add(Duration::from_millis(wait_ms));
        let run = self.run_of(id);
        let parked = match run {
            Some(run) => self.park_until(id, run, until, cancel),
            // Gone between the typer and the park: nothing to wait for.
            None => Parked::Timeout,
        };
        // A cancel stops the typing between chunks.
        let mut text = if written < input.len() {
            format!("Wrote {written} of {} bytes.\n", input.len())
        } else {
            String::new()
        };
        text.push_str(&since_text(&path, from));
        let state = match (parked, run) {
            (Parked::Ended, Some(run)) => self
                .answer_for(id, run)
                .unwrap_or_else(|| self.still_running(id)),
            _ => self.still_running(id),
        };
        text.push_str(&state.text);
        Ok(Answer {
            text,
            settle: state.settle,
        })
    }

    /// The job's typing closure and output file.
    fn typer_of(&self, id: &str) -> Result<(Typer, PathBuf), WriteError> {
        let inner = lock(&self.inner);
        let Some(job) = inner.jobs.iter().find(|job| job.started.job_id.0 == id) else {
            return Err(WriteError::NotRunning);
        };
        if let Phase::Ended(completed) = &job.phase {
            return Err(WriteError::Ended(completed.status));
        }
        match &job.input {
            Some(typer) => Ok((Arc::clone(typer), job.path.clone())),
            None => Err(WriteError::NotTty),
        }
    }

    /// The stop closure with its run, when the job is running: `None`
    /// for the closure when a stop was already sent for this run. `Err`
    /// when the job is unknown or has ended.
    fn send_stop(&self, id: &str) -> Result<RunStop, StopError> {
        let mut inner = lock(&self.inner);
        let Some(job) = inner.jobs.iter_mut().find(|job| job.started.job_id.0 == id) else {
            return Err(StopError::NotRunning);
        };
        if let Phase::Ended(completed) = &job.phase {
            return Err(StopError::Ended(completed.status));
        }
        let run = job.run;
        if job.stop_sent {
            return Ok((None, run));
        }
        job.stop_sent = true;
        Ok((Some(Arc::clone(&job.stop)), run))
    }

    fn still_running(&self, id: &str) -> Answer {
        let inner = lock(&self.inner);
        let path = inner
            .jobs
            .iter()
            .find(|job| job.started.job_id.0 == id)
            .map(|job| job.path.clone());
        let text = match path {
            Some(path) => running_text(id, &path),
            None => format!("Job {id} is still running.\n"),
        };
        Answer { text, settle: None }
    }

    /// Parks until run `run` of `id` ends, `until` passes, or `cancel`
    /// fires. `until` of `None` waits without a deadline. The `Arc` stays
    /// alive for the park: a cancel subscribed here upgrades it.
    fn park_until(
        self: &Arc<Self>,
        id: &str,
        run: u64,
        until: Option<Instant>,
        cancel: &dyn Cancel,
    ) -> Parked {
        let wake = Arc::clone(self) as Arc<dyn Wake>;
        park_until(
            &self.park,
            self.clock.as_ref(),
            cancel,
            until,
            &wake,
            || {
                let inner = lock(&self.inner);
                if !runs::is_running(&inner, id, run) {
                    return Some(Parked::Ended);
                }
                if cancel.is_cancelled() {
                    return Some(Parked::Cancelled);
                }
                if timed_out(self.clock.as_ref(), until) {
                    return Some(Parked::Timeout);
                }
                None
            },
        )
    }
}

impl Wake for Registry {
    fn wake(&self) {
        self.park.bump();
    }
}

fn timed_out(clock: &dyn Clock, until: Option<Instant>) -> bool {
    until.is_some_and(|until| clock.now() >= until)
}

/// `j_` and 16 lowercase hex digits, drawn once. 64 bits, the same scheme
/// as the loop's ids.
fn mint_id() -> String {
    support::mint("j_")
}

/// Mints the id before spawning, so the launch and the record name the
/// same delegate job.
pub(crate) fn mint_job_id() -> JobId {
    JobId(mint_id())
}

/// One listing line, in start order. An ended Fiber delegate carries
/// its stored failure message after its entry, so a failure before its
/// session exists still names its cause. Its log path is shown unless
/// the filesystem reports it missing, in which case the path and its
/// separator go together: anything else keeps its recorded path, and
/// only a missing log is evidence of one.
fn entry_text(job: &Job) -> String {
    let mut line = format!(
        "{} {} {}",
        job.started.job_id.0,
        phase_word(&job.phase),
        job.started.description,
    );
    let failure = match (&job.phase, &job.delegate) {
        (Phase::Ended(completed), Some(started)) if started.harness == "fiber" => {
            completed.error.as_ref()
        }
        _ => None,
    };
    let missing = failure.is_some() && log_missing(&job.path);
    if !missing {
        line.push_str(&format!(" \u{2014} {}", job.path.display()));
    }
    if let Some(error) = failure {
        line.push_str(&format!(" \u{2014} {}", error.message));
    }
    line.push('\n');
    line
}

/// Whether `path` is missing: only a missing file is evidence of a
/// missing log. Any other metadata failure, such as an unreadable
/// directory on its way, keeps the recorded path.
fn log_missing(path: &Path) -> bool {
    matches!(
        std::fs::metadata(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

fn phase_word(phase: &Phase) -> &'static str {
    match phase {
        Phase::Running => "running",
        Phase::Ended(completed) => status_word(completed.status),
    }
}

/// The word for a job that has ended: `completed`, `failed` or `cancelled`.
pub(crate) fn status_word(status: Outcome) -> &'static str {
    match status {
        Outcome::Completed => "completed",
        Outcome::Failed => "failed",
        Outcome::Cancelled => "cancelled",
    }
}

/// The file's bytes from `from`, decoded lossily and ending in a newline
/// when there are any. `from` can fall inside a character, so a start past
/// the file's beginning drops the continuation bytes it begins with.
fn since_text(path: &Path, from: u64) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(from)).is_ok() {
        let _read = file.read_to_end(&mut bytes);
    }
    if from > 0 {
        let begin = bytes
            .iter()
            .position(|byte| byte & 0b1100_0000 != 0b1000_0000)
            .unwrap_or(bytes.len());
        bytes.drain(..begin);
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn running_text(id: &str, path: &Path) -> String {
    format!("Job {id} is still running.\nOutput: {}\n", path.display())
}

fn final_text(
    path: &Path,
    completed: &JobCompleted,
    finished: Option<&DelegateFinished>,
) -> String {
    let mut text = format!(
        "Job {} {}.\n",
        completed.job_id.0,
        status_word(completed.status)
    );
    if let Some(code) = completed
        .process
        .as_ref()
        .and_then(|process| process.exit_code)
    {
        text.push_str(&format!("Exit code {code}.\n"));
    }
    if let Some(signal) = completed
        .process
        .as_ref()
        .and_then(|process| process.signal.as_deref())
    {
        text.push_str(&format!("Killed by {signal}.\n"));
    }
    if let Some(error) = &completed.error {
        text.push_str(&error.message);
        if !error.message.ends_with('\n') {
            text.push('\n');
        }
    }
    if let Some(finished) = finished {
        // A delegate's final message rides its `wait` answer, as it rides
        // its wake.
        text.push_str("Final message:\n");
        text.push_str(&finished.text);
        if !finished.text.ends_with('\n') {
            text.push('\n');
        }
    }
    text.push_str(&format!("Output: {}\n", path.display()));
    if let Some(tail) = &completed.output_tail {
        text.push_str("Last output:\n");
        text.push_str(tail);
        if !tail.ends_with('\n') {
            text.push('\n');
        }
    }
    text
}

impl contract::jobs::Jobs for Registry {
    fn open(&self, opening: Opening) -> Result<Opened, OpenError> {
        Registry::open(self, opening)
    }

    /// Writes `text` to a running job started with `tty`, waiting at most
    /// [`WRITE_TIMEOUT`] on the session clock for the terminal to take it:
    /// a job that never reads stdin cannot hold the connection's reader
    /// thread past that wait (`docs/architecture.md`, "One inbox").
    fn write(&self, job_id: &JobId, text: &str) -> Result<(), contract::jobs::WriteError> {
        let (typer, _) = self.typer_of(&job_id.0)?;
        // `checked_add` is `None` when the wait does not fit on the clock.
        // That write has no deadline, as a `wait` with no deadline does.
        let until = self.clock.now().checked_add(WRITE_TIMEOUT);
        let cancel = Deadline {
            clock: Arc::clone(&self.clock),
            until,
        };
        match typer(text.as_bytes(), self.clock.as_ref(), &cancel) {
            Ok(written) if written >= text.len() => Ok(()),
            Ok(_) => Err(write_timed_out()),
            Err(error) => Err(contract::jobs::WriteError::Io(error)),
        }
    }

    fn stop(&self, job_id: &JobId) -> bool {
        match self.send_stop(&job_id.0) {
            Ok((Some(stop), _)) => {
                stop();
                true
            }
            Ok((None, _)) => true,
            Err(StopError::NotRunning | StopError::Ended(_)) => false,
        }
    }

    /// Sends each running delegate its stop, once, and returns how many
    /// were sent one. Ordinary jobs keep running; ended delegates send
    /// nothing. Latches a stop-all under the same lock first, even when
    /// nothing is registered: a delegate start racing this call commits
    /// after it and observes the latch at commit, so the one-way trip
    /// cannot miss a spawned-but-unregistered delegate. The latch never
    /// clears; a sent stop is never sent again.
    fn stop_delegates(&self) -> usize {
        let stops: Vec<Arc<dyn Fn() + Send + Sync>> = {
            let mut inner = lock(&self.inner);
            inner.stop_all_delegates = true;
            inner
                .jobs
                .iter_mut()
                .filter(|job| job.delegate.is_some() && !job.stop_sent)
                .filter(|job| matches!(job.phase, Phase::Running))
                .map(|job| {
                    job.stop_sent = true;
                    Arc::clone(&job.stop)
                })
                .collect()
        };
        for stop in &stops {
            stop();
        }
        stops.len()
    }

    fn background(&self) -> usize {
        // The closures run with the lock released: each takes its call's
        // own lock.
        let calls: Vec<_> = {
            let inner = lock(&self.inner);
            inner.foreground.iter().filter_map(Weak::upgrade).collect()
        };
        calls.iter().filter(|call| call()).count()
    }

    fn foreground(&self, call: Foreground) {
        let mut inner = lock(&self.inner);
        inner.foreground.retain(|call| call.strong_count() > 0);
        inner.foreground.push(call.0);
    }

    fn running(&self) -> Vec<JobId> {
        lock(&self.inner)
            .jobs
            .iter()
            .filter(|job| matches!(job.phase, Phase::Running))
            .map(|job| job.started.job_id.clone())
            .collect()
    }

    fn deliver_to(&self, inbox: Sender<Delivery>) {
        lock(&self.inner).inbox = Some(inbox);
    }

    /// Sets the closed-starts flag under `start_gate`, before the end
    /// path's first `running()` read. A second call changes nothing. Calls
    /// nothing while it holds either lock.
    fn close_extension_starts(&self) {
        let _gate = lock(&self.start_gate);
        lock(&self.inner).extension_starts_closed = true;
    }

    fn install_usage_tap(&self, tap: Option<contract::jobs::UsageTap>) {
        lock(&self.inner).usage_tap = tap;
    }
}

fn lock<T>(inner: &Mutex<T>) -> MutexGuard<'_, T> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long [`contract::jobs::Jobs::write`] waits on the session clock for
/// the terminal to take the input before it reports the timeout. Picked,
/// not measured.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// The timeout [`contract::jobs::Jobs::write`] reports when the terminal
/// did not take the input within [`WRITE_TIMEOUT`], whole or in part.
fn write_timed_out() -> contract::jobs::WriteError {
    contract::jobs::WriteError::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "The job's terminal did not take the input.",
    ))
}

/// A cancel that fires once `until` passes on the session clock, bounding
/// [`contract::jobs::Jobs::write`]. `None` never fires.
struct Deadline {
    clock: Arc<dyn Clock>,
    until: Option<Instant>,
}

impl Cancel for Deadline {
    fn is_cancelled(&self) -> bool {
        self.until.is_some_and(|until| self.clock.now() >= until)
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        self.clock.subscribe(waker);
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
