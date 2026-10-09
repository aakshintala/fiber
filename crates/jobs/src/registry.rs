//! The session's jobs: one id, one output file, one lifecycle
//! (`docs/tools.md`, "Background jobs").

use std::collections::hash_map::RandomState;
use std::fs::File;
use std::hash::BuildHasher;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{DelegateFinished, JobCompleted, JobLine, JobStarted, Outcome};
use contract::inbox::{Claim, Delivery, JobNotice};
use contract::jobs::{End, Foreground, JobRecord, Lines, OpenError, Opened, Opening, Stop};
use contract::shapes::Failure;
use contract::tool::Cancel;
use contract::{ErrorCode, JobId};

#[path = "registry/park.rs"]
mod park;

use park::{Parked, Parker};

/// A job's end not yet reported, held by the closure in its [`End`] or
/// [`Finish`]. Reporting consumes it; dropped unreported, it records the
/// job failed `indeterminate`, since a runner that returned without
/// reporting would otherwise leave the job running. A panic aborts the
/// process; this is not a panic handler.
struct Unreported {
    job_id: JobId,
    /// Taken by the one report.
    registry: Option<Arc<Registry>>,
    /// Whether this is a delegate: its drop carries an empty finish.
    delegate: bool,
}

impl Unreported {
    /// Records `completed` for this job, whatever id the payload names: a
    /// payload that names another id would record the wrong job, or none.
    fn report(mut self, completed: JobCompleted, delegate: Option<DelegateFinished>) {
        if let Some(registry) = self.registry.take() {
            registry.finish(
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
            let delegate = self.delegate.then(|| empty_finished(&self.job_id));
            registry.finish(
                JobCompleted {
                    job_id: self.job_id.clone(),
                    status: Outcome::Failed,
                    error: Some(Failure {
                        code: ErrorCode::Indeterminate,
                        message: "The job ended without a result.".to_owned(),
                        retry_after_ms: None,
                        provider: None,
                    }),
                    process: None,
                    output_tail: None,
                },
                delegate,
            );
        }
    }
}

/// Reports how a delegate ended, once: its completion and, when the run
/// produced one, its finish. Dropped unreported, it records the job failed
/// `indeterminate` with an empty finish, as [`End`] does for other jobs.
/// The runner (task 3.3) is its only caller.
pub(crate) struct Finish(Unreported);

impl Finish {
    /// Records the delegate's end. The payload's id is replaced with the
    /// recorded one, as [`Unreported::report`] does.
    pub(crate) fn report(self, completed: JobCompleted, delegate: Option<DelegateFinished>) {
        self.0.report(completed, delegate);
    }
}

/// A delegate that ended without reporting one: empty text, zero usage.
fn empty_finished(job_id: &JobId) -> DelegateFinished {
    DelegateFinished {
        job_id: job_id.clone(),
        text: String::new(),
        artifact: None,
        questions: None,
        usage: contract::shapes::Usage {
            tokens: contract::shapes::Tokens {
                input: 0,
                cache_read: 0,
                cache_write: std::collections::BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        worktree: None,
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
    /// Upgrades to the `Arc` `new` returned, so `open` can hand that `Arc`
    /// to the job's [`End`] while taking `&self`.
    me: Weak<Self>,
}

struct Inner {
    jobs: Vec<Job>,
    /// The loop's inbox, which a job's end is sent to. `None` until
    /// [`contract::jobs::Jobs::deliver_to`].
    inbox: Option<Sender<Delivery>>,
    /// Running foreground calls, held weakly (`Jobs::foreground`).
    foreground: Vec<Weak<dyn Fn() -> bool + Send + Sync>>,
}

struct Job {
    started: JobStarted,
    path: PathBuf,
    phase: Phase,
    stop: Arc<dyn Fn() + Send + Sync>,
    /// Types into the job's terminal; `None` unless it started with `tty`.
    input: Option<Typer>,
    stop_sent: bool,
    claimed: bool,
    /// Whether this is a Fiber delegate: only `delegate_spawn` opens one.
    /// The runner marks it; `stop_delegates` reads it.
    delegate: bool,
    /// A delegate's finish, kept past the end for a later `wait`.
    finished: Option<DelegateFinished>,
}

type Typer = Arc<dyn Fn(&[u8], &dyn Clock, &dyn Cancel) -> std::io::Result<usize> + Send + Sync>;

enum Phase {
    Running,
    /// Boxed: `JobCompleted` is far larger than `Running`, and a registry
    /// holds one phase per job.
    Ended(Box<JobCompleted>),
}

/// What a `wait` or a `stop` that reached a job returns to the model.
pub(crate) struct Answer {
    /// The lines the model sees.
    pub(crate) text: String,
    /// The records, the first time they are delivered: a delegate's
    /// finish, when it has one, then its completion.
    pub(crate) records: Vec<JobRecord>,
}

/// `write` could not reach the job.
pub(crate) enum WriteError {
    /// No job has this id.
    Unknown,
    /// The job was not started with `tty`.
    NotTty,
    /// The job had already ended, with this status.
    Ended(Outcome),
    /// Writing to the terminal failed.
    Io(std::io::Error),
}

/// `stop` could not ask the job to stop.
pub(crate) enum StopError {
    /// No job has this id.
    Unknown,
    /// The job had already ended, with this status.
    Ended(Outcome),
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
                foreground: Vec::new(),
            }),
            park: Parker::new(),
            me: Weak::clone(me),
        });
        let wake: Arc<dyn Wake> = registry.clone();
        clock.subscribe(Arc::downgrade(&wake));
        registry
    }

    /// Mints the id, creates the empty output file, and records the job
    /// running. Does not create `artifacts`: when the file cannot be
    /// created, returns [`OpenError::Io`] and records nothing.
    pub fn open(&self, opening: Opening) -> Result<Opened, OpenError> {
        let Some(registry) = self.me.upgrade() else {
            return Err(OpenError::Io {
                path: self.artifacts.clone(),
                source: std::io::Error::other("the registry is gone"),
            });
        };
        let mut inner = lock(&self.inner);
        let id = mint_id();
        let path = self.artifacts.join(format!("{id}.log"));
        let file = match File::create(&path) {
            Ok(file) => file,
            Err(source) => return Err(OpenError::Io { path, source }),
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
            registry: Some(registry),
            delegate: false,
        };
        let end = End(Box::new(move |completed| {
            unreported.report(completed, None)
        }));
        inner.jobs.push(Job {
            started: started.clone(),
            path: path.clone(),
            phase: Phase::Running,
            stop: Arc::from(opening.stop.0),
            input: opening.input.map(|input| Arc::from(input.0)),
            stop_sent: false,
            claimed: false,
            delegate: false,
            finished: None,
        });
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
    /// Only `delegate_spawn` opens one. The id is minted up front with
    /// [`mint_job_id`], so the spawn and the record name the same job.
    pub(crate) fn open_started(
        self: &Arc<Self>,
        job_id: JobId,
        tool: String,
        description: String,
        output_path: String,
        stop: Stop,
    ) -> (JobStarted, Finish) {
        let started = JobStarted {
            job_id: job_id.clone(),
            tool: Some(tool),
            extension: None,
            description,
            output_path: output_path.clone(),
        };
        let finish = Finish(Unreported {
            job_id: job_id.clone(),
            // The caller holds this `Arc`, so the job it records always
            // has a registry to end in.
            registry: Some(Arc::clone(self)),
            delegate: true,
        });
        lock(&self.inner).jobs.push(Job {
            started: started.clone(),
            path: PathBuf::from(output_path),
            phase: Phase::Running,
            stop: Arc::from(stop.0),
            input: None,
            stop_sent: false,
            claimed: false,
            delegate: true,
            finished: None,
        });
        (started, finish)
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
            text.push_str(&format!(
                "{} {} {} \u{2014} {}\n",
                job.started.job_id.0,
                phase_word(&job.phase),
                job.started.description,
                job.path.display()
            ));
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

    /// Blocks until `id` ends, `timeout_ms` passes, or `cancel` fires.
    /// A job that has already ended returns at once. A timeout or a cancel
    /// leaves the job running.
    pub(crate) fn wait(
        self: &Arc<Self>,
        id: &str,
        timeout_ms: u64,
        cancel: &dyn Cancel,
    ) -> Result<Answer, ()> {
        if !self.contains(id) {
            return Err(());
        }
        if let Some(answer) = self.answer_if_ended(id) {
            return Ok(answer);
        }
        // `checked_add` is `None` when the timeout does not fit on the
        // clock. That wait has no deadline: it runs until the job ends or
        // the wait is cancelled.
        let until = self
            .clock
            .now()
            .checked_add(Duration::from_millis(timeout_ms));
        match self.park_until(id, until, cancel) {
            Parked::Ended => Ok(self
                .answer_if_ended(id)
                .unwrap_or_else(|| self.still_running(id))),
            Parked::Timeout | Parked::Cancelled => Ok(self.still_running(id)),
        }
    }

    /// Asks `id` to stop, once, then waits until it ends or `cancel` fires.
    /// A cancel returns at once; the stop already sent stands.
    pub(crate) fn stop(
        self: &Arc<Self>,
        id: &str,
        cancel: &dyn Cancel,
    ) -> Result<Answer, StopError> {
        if let Some(stop) = self.send_stop(id)? {
            stop();
        }
        if let Some(answer) = self.answer_if_ended(id) {
            return Ok(answer);
        }
        match self.park_until(id, None, cancel) {
            Parked::Ended => Ok(self
                .answer_if_ended(id)
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
        let parked = self.park_until(id, until, cancel);
        // A cancel stops the typing between chunks.
        let mut text = if written < input.len() {
            format!("Wrote {written} of {} bytes.\n", input.len())
        } else {
            String::new()
        };
        text.push_str(&since_text(&path, from));
        let state = match parked {
            Parked::Ended => self
                .answer_if_ended(id)
                .unwrap_or_else(|| self.still_running(id)),
            Parked::Timeout | Parked::Cancelled => self.still_running(id),
        };
        text.push_str(&state.text);
        Ok(Answer {
            text,
            records: state.records,
        })
    }

    /// The job's typing closure and output file.
    fn typer_of(&self, id: &str) -> Result<(Typer, PathBuf), WriteError> {
        let inner = lock(&self.inner);
        let Some(job) = inner.jobs.iter().find(|job| job.started.job_id.0 == id) else {
            return Err(WriteError::Unknown);
        };
        if let Phase::Ended(completed) = &job.phase {
            return Err(WriteError::Ended(completed.status));
        }
        match &job.input {
            Some(typer) => Ok((Arc::clone(typer), job.path.clone())),
            None => Err(WriteError::NotTty),
        }
    }

    /// The stop closure, when this call is the one that sends it. `None`
    /// when a stop was already sent. `Err` when the job is unknown or has
    /// ended.
    fn send_stop(&self, id: &str) -> Result<Option<Arc<dyn Fn() + Send + Sync>>, StopError> {
        let mut inner = lock(&self.inner);
        let Some(job) = inner.jobs.iter_mut().find(|job| job.started.job_id.0 == id) else {
            return Err(StopError::Unknown);
        };
        if let Phase::Ended(completed) = &job.phase {
            return Err(StopError::Ended(completed.status));
        }
        if job.stop_sent {
            return Ok(None);
        }
        job.stop_sent = true;
        Ok(Some(Arc::clone(&job.stop)))
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
        Answer {
            text,
            records: Vec::new(),
        }
    }

    /// The final state, claiming its records the first time: a delegate's
    /// finish, when it has one, then its completion. `None` when the job
    /// is still running.
    fn answer_if_ended(&self, id: &str) -> Option<Answer> {
        let mut inner = lock(&self.inner);
        let job = inner
            .jobs
            .iter_mut()
            .find(|job| job.started.job_id.0 == id)?;
        let completed = match &job.phase {
            Phase::Ended(completed) => (**completed).clone(),
            Phase::Running => return None,
        };
        let text = final_text(&job.path, &completed, job.finished.as_ref());
        let records = if job.claimed {
            Vec::new()
        } else {
            job.claimed = true;
            let mut records = Vec::with_capacity(2);
            if let Some(finished) = job.finished.clone() {
                records.push(JobRecord::DelegateFinished(finished));
            }
            records.push(JobRecord::Completed(completed));
            records
        };
        Some(Answer { text, records })
    }

    fn finish(&self, completed: JobCompleted, delegate: Option<DelegateFinished>) {
        let mut guard = lock(&self.inner);
        let inner = &mut *guard;
        if let Some(job) = inner
            .jobs
            .iter_mut()
            .find(|job| job.started.job_id.0 == completed.job_id.0)
        {
            // The borrow from `matches!` ends at this statement, so the
            // assignment below can replace `phase`.
            let running = matches!(job.phase, Phase::Running);
            if running {
                job.phase = Phase::Ended(Box::new(completed.clone()));
                // The terminal closes with the job.
                job.input = None;
                // A delegate's finish is kept past the end, for a later
                // `wait` and its text.
                job.finished = delegate.clone();
                if let Some(inbox) = &inner.inbox {
                    let registry = Weak::clone(&self.me);
                    let id = completed.job_id.0.clone();
                    let claim = Claim(Box::new(move || {
                        registry
                            .upgrade()
                            .is_some_and(|registry| registry.claim(&id))
                    }));
                    // A loop that is gone takes no news; the end stays
                    // recorded for `list`.
                    let _sent = inbox.send(Delivery::Job(JobNotice {
                        completed,
                        claim,
                        delegate,
                    }));
                }
            }
        }
        self.park.bump();
    }

    /// Claims `id`'s final state for one caller: true the first time, false
    /// once a notice, a `wait` or a `stop` claimed it.
    fn claim(&self, id: &str) -> bool {
        let mut inner = lock(&self.inner);
        let Some(job) = inner.jobs.iter_mut().find(|job| job.started.job_id.0 == id) else {
            return false;
        };
        !std::mem::replace(&mut job.claimed, true)
    }

    /// Parks until `id` ends, `until` passes, or `cancel` fires. `until`
    /// of `None` waits without a deadline. The `Arc` stays alive for the
    /// park: a cancel subscribed here upgrades it.
    fn park_until(
        self: &Arc<Self>,
        id: &str,
        until: Option<Instant>,
        cancel: &dyn Cancel,
    ) -> Parked {
        let wake = Arc::clone(self) as Arc<dyn Wake>;
        self.park
            .park_until(self.clock.as_ref(), cancel, until, &wake, || {
                let inner = lock(&self.inner);
                if ended(&inner, id) {
                    return Some(Parked::Ended);
                }
                if cancel.is_cancelled() {
                    return Some(Parked::Cancelled);
                }
                if timed_out(self.clock.as_ref(), until) {
                    return Some(Parked::Timeout);
                }
                None
            })
    }
}

impl Wake for Registry {
    fn wake(&self) {
        self.park.bump();
    }
}

fn ended(inner: &Inner, id: &str) -> bool {
    inner
        .jobs
        .iter()
        .any(|job| job.started.job_id.0 == id && matches!(job.phase, Phase::Ended(_)))
}

fn timed_out(clock: &dyn Clock, until: Option<Instant>) -> bool {
    until.is_some_and(|until| clock.now() >= until)
}

/// `j_` and 16 lowercase hex digits, drawn once. 64 bits, the same scheme
/// as the loop's ids.
fn mint_id() -> String {
    format!("j_{:016x}", RandomState::new().hash_one(()))
}

/// Mints the id before spawning, so the launch and the record name the
/// same delegate job.
pub(crate) fn mint_job_id() -> JobId {
    JobId(mint_id())
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

    /// Writes `text` to a running job started with `tty`, at once, with a
    /// cancel that never fires: no wait for output, as the driver command
    /// is accepted once written.
    fn write(&self, job_id: &JobId, text: &str) -> Result<(), contract::jobs::WriteError> {
        let (typer, _) = match self.typer_of(&job_id.0) {
            Ok(found) => found,
            Err(WriteError::Unknown | WriteError::Ended(_)) => {
                return Err(contract::jobs::WriteError::NotRunning);
            }
            Err(WriteError::NotTty) => return Err(contract::jobs::WriteError::NotTty),
            Err(WriteError::Io(error)) => return Err(contract::jobs::WriteError::Io(error)),
        };
        typer(text.as_bytes(), self.clock.as_ref(), &Never)
            .map(|_| ())
            .map_err(contract::jobs::WriteError::Io)
    }

    fn stop(&self, job_id: &JobId) -> bool {
        match self.send_stop(&job_id.0) {
            Ok(Some(stop)) => {
                stop();
                true
            }
            Ok(None) => true,
            Err(StopError::Unknown | StopError::Ended(_)) => false,
        }
    }

    /// Sends each running delegate its stop, once, and returns how many
    /// were sent one. Ordinary jobs keep running; ended delegates send
    /// nothing.
    fn stop_delegates(&self) -> usize {
        let stops: Vec<Arc<dyn Fn() + Send + Sync>> = {
            let mut inner = lock(&self.inner);
            inner
                .jobs
                .iter_mut()
                .filter(|job| job.delegate && !job.stop_sent)
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
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A cancel that never fires: [`contract::jobs::Jobs::write`] types at
/// once, with no wait for output.
struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
