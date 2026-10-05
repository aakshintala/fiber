//! Moving a running command onto a job (`docs/tools.md`, "Moving to the background").
//! The call thread opens the job and returns the receipt. The job's thread
//! runs the command to the end.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{JobCompleted, JobStarted, Outcome};
use contract::jobs::{Foreground, JobRecord, Jobs, OpenError, Opened, Opening, Stop};
use contract::shapes::ContentPart;
use contract::tool::{Cancel, Output};
use contract::{ErrorCode, JobId};

use super::command::{Finished, MovePolicy, MoveReason, Moved, StopKind};
use super::monitor::Feed;
use super::output::JobStream;
use super::tty;
use super::{Limit, assemble};

/// A description longer than this is cut, with `…` as the last character.
/// Picked, not measured.
const DESCRIPTION_LIMIT: usize = 80;

/// `output_tail` keeps this many bytes. Picked, not measured.
const TAIL_BYTES: usize = 2_048;

/// `ps` lists the group within this, or the receipt names no one. Picked,
/// not measured.
const PS_BOUND: Duration = Duration::from_secs(2);

/// How often `ps`'s exit is checked once its output closed. Picked, not
/// measured.
const PS_POLL: Duration = Duration::from_millis(10);

const UNNAMED: &str = "Processes are still running in its group.";

const CAPPED: &str = "The output passed 5 GB and the job was stopped.";

const FLOODED: &str = "The monitor's output was suppressed for 30 seconds, so it was stopped. \
     Restart it with a more selective source.";

const AFTER_THIRTY: &str = "Still running after 30 seconds, so it moved to the background.";
const STARTED: &str = "Started in the background.";
const IN_TERMINAL: &str = "Started in a terminal and moved to the background.";
const COMMANDED: &str = "Moved to the background by the `background` command.";

/// What the running phase does on this pass. Timeout wins over a move at
/// the same instant; a shell that left members wins over moving at once.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Stop the group.
    Stop(StopKind),
    /// The group is empty. Finish in the foreground.
    Drain,
    /// Hand the command to a job.
    Move(MoveReason),
    /// Wait for the next wake.
    Park,
}

/// `shell_exit` is the shell's exit code once it was reaped. `asked` is the
/// `background` command's request (`docs/invocation.md`, "Driver commands").
pub(crate) fn running_step(
    policy: MovePolicy,
    timed_out: bool,
    cancelled: bool,
    seen_empty: bool,
    shell_exit: Option<i32>,
    move_due: bool,
    asked: bool,
) -> Step {
    if timed_out {
        return Step::Stop(StopKind::Timeout);
    }
    if cancelled {
        return Step::Stop(StopKind::Cancel);
    }
    if seen_empty {
        return Step::Drain;
    }
    match policy {
        MovePolicy::Stay => Step::Park,
        MovePolicy::Foreground
        | MovePolicy::Background
        | MovePolicy::Terminal
        | MovePolicy::Monitor => {
            if let Some(code) = shell_exit {
                Step::Move(MoveReason::ShellExited { code })
            } else if matches!(
                policy,
                MovePolicy::Background | MovePolicy::Terminal | MovePolicy::Monitor
            ) {
                Step::Move(MoveReason::StartedInBackground)
            } else if asked && policy == MovePolicy::Foreground {
                Step::Move(MoveReason::BackgroundCommand)
            } else if move_due {
                Step::Move(MoveReason::AfterThirtySeconds)
            } else {
                Step::Park
            }
        }
    }
}

pub(crate) fn wait_deadline(
    policy: MovePolicy,
    timeout_at: Option<Instant>,
    move_at: Option<Instant>,
) -> Option<Instant> {
    match policy {
        MovePolicy::Stay | MovePolicy::Background | MovePolicy::Terminal | MovePolicy::Monitor => {
            timeout_at
        }
        MovePolicy::Foreground => match (timeout_at, move_at) {
            (Some(timeout_at), Some(move_at)) => Some(timeout_at.min(move_at)),
            (Some(instant), None) | (None, Some(instant)) => Some(instant),
            (None, None) => None,
        },
    }
}

/// Where the `background` command's request to one foreground call stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ask {
    /// Waiting in the foreground, not asked.
    Waiting,
    /// Asked to move.
    Asked,
    /// Stopping, finished or moved: a request moves nothing.
    Over,
}

/// One foreground call's move request (`docs/invocation.md`, "Driver
/// commands"). The call registers it with the jobs and holds the closure
/// while it waits in the foreground; the drive loop reads [`MoveAsk::asked`].
pub(super) struct MoveAsk {
    state: Mutex<Ask>,
    /// Wakes the drive loop, which then reads [`MoveAsk::asked`].
    wake: Weak<dyn Wake>,
}

type Request = Arc<dyn Fn() -> bool + Send + Sync>;

impl MoveAsk {
    /// Registers a foreground call with `jobs`. `wake` wakes its drive
    /// loop. The caller holds the returned closure for as long as the call
    /// waits in the foreground: the jobs hold it weakly.
    pub(super) fn register(jobs: &dyn Jobs, wake: Weak<dyn Wake>) -> (Arc<Self>, Request) {
        let ask = Arc::new(Self {
            state: Mutex::new(Ask::Waiting),
            wake,
        });
        let this = Arc::clone(&ask);
        let request: Request = Arc::new(move || this.request());
        jobs.foreground(Foreground(Arc::downgrade(&request)));
        (ask, request)
    }

    /// True when the call was still in the foreground and will now move.
    fn request(&self) -> bool {
        {
            let mut state = lock_state(&self.state);
            if *state == Ask::Over {
                return false;
            }
            *state = Ask::Asked;
        }
        // After the state is set, so the loop that wakes reads it.
        if let Some(wake) = self.wake.upgrade() {
            wake.wake();
        }
        true
    }

    pub(super) fn asked(&self) -> bool {
        *lock_state(&self.state) == Ask::Asked
    }

    /// Later requests move nothing.
    pub(super) fn end(&self) {
        *lock_state(&self.state) = Ask::Over;
    }
}

fn lock_state(state: &Mutex<Ask>) -> MutexGuard<'_, Ask> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Opens the job and either returns its receipt or, when open fails, the
/// foreground result with one line naming the failure. `terminal` is a
/// `tty` command: its receipt waits for the first output.
#[allow(
    clippy::too_many_arguments,
    reason = "the moved command, the jobs, the clock and the call's own cancel and emit are one hand-off"
)]
pub(crate) fn take(
    moved: Moved,
    jobs: Arc<dyn Jobs>,
    clock: Arc<dyn Clock>,
    command: &str,
    limit: Limit,
    terminal: bool,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
) -> Output {
    let job_cancel = JobCancel::new();
    match jobs.open(Opening {
        tool: "shell".to_owned(),
        description: description_of(command),
        stop: job_cancel.stop(),
        input: moved.take_input(),
        lines: limit.monitor,
    }) {
        Ok(opened) => hand_off(moved, opened, job_cancel, clock, limit, terminal, cancel),
        Err(error) => {
            let finished = moved.resume(clock.as_ref(), cancel, emit);
            note_open_failure(assemble(limit, finished), &error)
        }
    }
}

fn hand_off(
    mut moved: Moved,
    opened: Opened,
    job_cancel: Arc<JobCancel>,
    clock: Arc<dyn Clock>,
    limit: Limit,
    terminal: bool,
    cancel: &dyn Cancel,
) -> Output {
    let pgid = moved.pgid();
    let shared = moved.shared();
    let reason = moved.reason.clone();
    moved.attach_output(opened.file);
    // Armed before the call's cancel is detached, so a stop that arrives
    // as soon as the job is recorded still wakes the drive loop.
    moved.arm(job_cancel.as_ref());
    moved.detach_call_cancel();
    let path = opened.path.clone();
    let job_id = opened.started.job_id.clone();
    let end = opened.end;
    let stream = JobStream::new(job_id.clone(), opened.emit);
    let errors = errors_path(&opened.path);
    // A monitor's standard error file, or why it could not be created.
    let mut sink = None;
    let feed = opened.lines.map(|lines| {
        sink = Some(match File::create(&errors) {
            Ok(file) => {
                moved.attach_errors(Some(file));
                Ok(())
            }
            Err(error) => {
                moved.attach_errors(None);
                Err(error)
            }
        });
        Feed::new(job_id.clone(), lines, job_cancel.stop(), clock.now())
    });
    let job_clock = Arc::clone(&clock);
    // The job's thread starts before the receipt lists the group, so its
    // timeout and stop hold while `ps` runs.
    thread::spawn(move || {
        let finished = moved.drive_job(job_clock.as_ref(), job_cancel.as_ref(), stream, feed);
        end.end(to_completed(job_id, &path, finished, limit));
    });
    if let Some(sink) = sink {
        return monitor_receipt(&opened.started, &opened.path, &errors, &sink, limit.ms);
    }
    // The receipt carries what the terminal printed in its first 250 ms.
    let first = terminal.then(|| {
        tty::wait_first_output(&shared, clock.as_ref(), cancel);
        tty::output_so_far(&opened.path)
    });
    let members = match reason {
        MoveReason::ShellExited { .. } => group_members(Path::new("ps"), pgid, clock.as_ref()),
        MoveReason::AfterThirtySeconds
        | MoveReason::StartedInBackground
        | MoveReason::BackgroundCommand => None,
    };
    match first {
        Some(first) => terminal_receipt(
            &opened.started,
            &opened.path,
            &reason,
            members.as_deref(),
            &first,
        ),
        None => receipt(&opened.started, &opened.path, &reason, members.as_deref()),
    }
}

/// A `tty` command's receipt: how to type into it, and `first`, what the
/// terminal printed before the receipt.
fn terminal_receipt(
    started: &JobStarted,
    path: &Path,
    reason: &MoveReason,
    members: Option<&str>,
    first: &str,
) -> Output {
    let why = if matches!(reason, MoveReason::StartedInBackground) {
        IN_TERMINAL.to_owned()
    } else {
        sentence(reason, members)
    };
    let mut text = format!(
        "{why}\nJob {id}. Output: {path}. Type into it with `jobs write`; `jobs wait` waits for it.\n",
        id = started.job_id.0,
        path = path.display(),
    );
    if !first.is_empty() {
        text.push_str("Output so far:\n");
        text.push_str(first);
        if !first.ends_with('\n') {
            text.push('\n');
        }
    }
    Output {
        content: vec![ContentPart::Text { text }],
        jobs: vec![JobRecord::Started(started.clone())],
        ..Output::default()
    }
}

/// A monitor's standard error file: beside its output file, named
/// `<job_id>.stderr.log`.
fn errors_path(output: &Path) -> std::path::PathBuf {
    let stem = output
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    output.with_file_name(format!("{stem}.stderr.log"))
}

/// A monitor's receipt: its files, and how its lines reach the model.
fn monitor_receipt(
    started: &JobStarted,
    path: &Path,
    errors: &Path,
    sink: &std::io::Result<()>,
    deadline_ms: u64,
) -> Output {
    let errors = match sink {
        Ok(()) => format!("Errors: {}.", errors.display()),
        Err(error) => format!(
            "Its standard error is discarded: {} could not be created ({error}).",
            errors.display()
        ),
    };
    let text = format!(
        "Started a monitor.\nJob {id}. Output: {path}. {errors} Lines it prints reach you in batches; its deadline is {deadline_ms} ms.\n",
        id = started.job_id.0,
        path = path.display(),
    );
    Output {
        content: vec![ContentPart::Text { text }],
        jobs: vec![JobRecord::Started(started.clone())],
        ..Output::default()
    }
}

/// `assemble`'s text always ends with a newline, so the line goes after it.
fn note_open_failure(mut output: Output, error: &OpenError) -> Output {
    if let Some(ContentPart::Text { text }) = output.content.first_mut() {
        text.push_str(&format!("It could not move to the background: {error}.\n"));
    }
    output
}

/// `members` names what a shell that exited left running.
fn receipt(
    started: &JobStarted,
    path: &Path,
    reason: &MoveReason,
    members: Option<&str>,
) -> Output {
    let why = sentence(reason, members);
    let text = format!(
        "{why}\nJob {id}. Output: {path}. Read it with `read`; `jobs wait` waits for it.\n",
        id = started.job_id.0,
        path = path.display(),
    );
    Output {
        content: vec![ContentPart::Text { text }],
        jobs: vec![JobRecord::Started(started.clone())],
        ..Output::default()
    }
}

fn sentence(reason: &MoveReason, members: Option<&str>) -> String {
    match reason {
        MoveReason::AfterThirtySeconds => AFTER_THIRTY.to_owned(),
        MoveReason::StartedInBackground => STARTED.to_owned(),
        MoveReason::BackgroundCommand => COMMANDED.to_owned(),
        MoveReason::ShellExited { code } => shell_sentence(*code, members),
    }
}

fn shell_sentence(code: i32, members: Option<&str>) -> String {
    match members {
        Some(names) => format!(
            "The shell exited with code {code}, leaving {names} running, so it moved to the background."
        ),
        None => {
            format!("The shell exited with code {code}, so it moved to the background. {UNNAMED}")
        }
    }
}

/// The group's members as `name (pid)`, listed by `program` (`ps`). `None`
/// when it lists none, fails, or has not finished within [`PS_BOUND`] on
/// `clock`; then only that `ps` is killed.
fn group_members(program: &Path, pgid: u32, clock: &dyn Clock) -> Option<String> {
    let mut child = Command::new(program)
        .args(["-A", "-o", "pid=,pgid=,comm="])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let (tx, rx) = mpsc::channel();
    let tick: Arc<dyn Wake> = Arc::new(Tick(tx.clone()));
    clock.subscribe(Arc::downgrade(&tick));
    let mut stdout = child.stdout.take();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(stdout) = stdout.as_mut() {
            let _read = stdout.read_to_end(&mut bytes);
        }
        let _sent = tx.send(Some(bytes));
    });
    let stdout = until_exited(&mut child, &rx, clock, clock.now().checked_add(PS_BOUND))?;
    let rows = parse_ps(&String::from_utf8_lossy(&stdout), pgid);
    (!rows.is_empty()).then(|| format_rows(&rows))
}

/// `Some(bytes)` is `ps`'s whole output; `None` says the clock moved.
type Listed = Option<Vec<u8>>;

/// Wakes [`until_exited`] when the clock moves.
struct Tick(mpsc::Sender<Listed>);

impl Wake for Tick {
    fn wake(&self) {
        let _sent = self.0.send(None);
    }
}

/// `ps`'s output once its pipe closed and it exited successfully, all
/// before `deadline` on `clock`; else `None`, and at the deadline only this
/// `ps` is killed. A clock at or past the deadline hands the wait a zero
/// bound. Once the pipe closed, the exit is polled every [`PS_POLL`].
fn until_exited(
    child: &mut Child,
    rx: &mpsc::Receiver<Listed>,
    clock: &dyn Clock,
    deadline: Option<Instant>,
) -> Option<Vec<u8>> {
    let mut listed = None;
    let mut due = false;
    loop {
        if listed.is_some()
            && let Ok(Some(status)) = child.try_wait()
        {
            return listed.filter(|_| status.success());
        }
        if due {
            // Std's `Child::kill` signals this `ps` alone, never a group.
            let _killed = child.kill();
            let _reaped = child.wait();
            return None;
        }
        clock.wait_until(deadline, &mut |bound| {
            due = bound == Some(Duration::ZERO);
            let wait = match listed {
                Some(_) => Some(bound.map_or(PS_POLL, |bound| bound.min(PS_POLL))),
                None => bound,
            };
            let received = match wait {
                Some(wait) => rx.recv_timeout(wait).ok(),
                None => rx.recv().ok(),
            };
            if let Some(Some(bytes)) = received {
                listed = Some(bytes);
            }
        });
    }
}

fn parse_ps(text: &str, pgid: u32) -> Vec<(u32, String)> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|word| word.parse::<u32>().ok()) else {
            continue;
        };
        let Some(group) = parts.next().and_then(|word| word.parse::<u32>().ok()) else {
            continue;
        };
        if group != pgid {
            continue;
        }
        let name = parts.collect::<Vec<_>>().join(" ");
        if name.is_empty() {
            continue;
        }
        rows.push((pid, name));
    }
    rows
}

fn format_rows(rows: &[(u32, String)]) -> String {
    rows.iter()
        .map(|(pid, name)| format!("{name} ({pid})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The command's first line, cut to [`DESCRIPTION_LIMIT`] characters.
fn description_of(command: &str) -> String {
    let line = command.lines().next().unwrap_or("");
    if line.chars().count() <= DESCRIPTION_LIMIT {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(DESCRIPTION_LIMIT - 1).collect();
    cut.push('\u{2026}');
    cut
}

/// The end maps as the foreground result does (`assemble`); a stop that
/// was not indeterminate is `cancelled`.
fn to_completed(job_id: JobId, path: &Path, finished: Finished, limit: Limit) -> JobCompleted {
    let capped = finished.capped;
    let flooded = finished.flooded;
    // The cap wins over every other end, a cancel included; a flood wins
    // over every other end but the cap.
    let stopped =
        !capped && !flooded && finished.stop == Some(StopKind::Cancel) && !finished.indeterminate;
    let mut output = assemble(limit, finished);
    if capped {
        output.error = Some(super::failure(ErrorCode::OutputCap, CAPPED.to_owned()));
    } else if flooded {
        output.error = Some(super::failure(ErrorCode::Flooded, FLOODED.to_owned()));
    }
    let status = if stopped {
        Outcome::Cancelled
    } else if output.error.is_some() {
        Outcome::Failed
    } else {
        Outcome::Completed
    };
    JobCompleted {
        job_id,
        status,
        output_tail: match status {
            Outcome::Failed => output_tail(path),
            Outcome::Completed | Outcome::Cancelled => None,
        },
        error: output.error,
        process: output.process,
    }
}

/// The last [`TAIL_BYTES`] of the file, read from there, decoded lossily.
fn output_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let window = u64::try_from(TAIL_BYTES).unwrap_or(u64::MAX);
    file.seek(SeekFrom::Start(len.saturating_sub(window)))
        .ok()?;
    let mut bytes = Vec::with_capacity(TAIL_BYTES);
    file.take(window).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(align_char_boundary(&bytes)).into_owned();
    if text.is_empty() { None } else { Some(text) }
}

/// Drops a partial character at the start of a cut, so the tail does not
/// begin with U+FFFD from a byte that belonged to the previous character.
fn align_char_boundary(slice: &[u8]) -> &[u8] {
    let start = slice
        .iter()
        .position(|byte| byte & 0b1100_0000 != 0b1000_0000)
        .unwrap_or(slice.len());
    slice.get(start..).unwrap_or_default()
}

struct JobInner {
    cancelled: bool,
    wakers: Vec<Weak<dyn Wake>>,
}

struct JobCancel {
    inner: Mutex<JobInner>,
}

impl JobCancel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(JobInner {
                cancelled: false,
                wakers: Vec::new(),
            }),
        })
    }

    fn stop(self: &Arc<Self>) -> Stop {
        let this = Arc::clone(self);
        Stop(Box::new(move || this.fire()))
    }

    fn fire(&self) {
        let wakers = {
            let mut inner = lock_job(&self.inner);
            inner.cancelled = true;
            std::mem::take(&mut inner.wakers)
        };
        for waker in wakers.into_iter().filter_map(|waker| waker.upgrade()) {
            waker.wake();
        }
    }
}

impl Cancel for JobCancel {
    fn is_cancelled(&self) -> bool {
        lock_job(&self.inner).cancelled
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        lock_job(&self.inner).wakers.push(waker);
    }
}

fn lock_job(inner: &Mutex<JobInner>) -> MutexGuard<'_, JobInner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod tests;
