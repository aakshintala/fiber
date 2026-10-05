//! Moving a running command onto a job (`docs/tools.md`, "Moving to the background").
//! The call thread opens the job and returns the receipt. The job's thread
//! runs the command to the end.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::JobId;
use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{JobCompleted, JobStarted, Outcome};
use contract::jobs::{JobRecord, Jobs, OpenError, Opened, Opening, Stop};
use contract::shapes::ContentPart;
use contract::tool::{Cancel, Output};

use super::assemble;
use super::command::{Finished, MovePolicy, MoveReason, Moved, StopKind};

/// A description longer than this is cut, with `…` as the last character.
/// Picked, not measured.
const DESCRIPTION_LIMIT: usize = 80;

/// `output_tail` keeps this many bytes. Picked, not measured.
const TAIL_BYTES: usize = 2_048;

/// `ps` lists the group within this, or the receipt names no one. Picked,
/// not measured.
const PS_BOUND: Duration = Duration::from_secs(2);

const UNNAMED: &str = "Processes are still running in its group.";

const AFTER_THIRTY: &str = "Still running after 30 seconds, so it moved to the background.";
const STARTED: &str = "Started in the background.";

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

/// `shell_exit` is the shell's exit code once it was reaped.
pub(crate) fn running_step(
    policy: MovePolicy,
    timed_out: bool,
    cancelled: bool,
    seen_empty: bool,
    shell_exit: Option<i32>,
    move_due: bool,
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
        MovePolicy::Foreground | MovePolicy::Background => {
            if let Some(code) = shell_exit {
                Step::Move(MoveReason::ShellExited { code })
            } else if policy == MovePolicy::Background {
                Step::Move(MoveReason::StartedInBackground)
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
        MovePolicy::Stay | MovePolicy::Background => timeout_at,
        MovePolicy::Foreground => match (timeout_at, move_at) {
            (Some(timeout_at), Some(move_at)) => Some(timeout_at.min(move_at)),
            (Some(instant), None) | (None, Some(instant)) => Some(instant),
            (None, None) => None,
        },
    }
}

/// Opens the job and either returns its receipt or, when open fails, the
/// foreground result with one line naming the failure.
pub(crate) fn take(
    moved: Moved,
    jobs: Arc<dyn Jobs>,
    clock: Arc<dyn Clock>,
    command: &str,
    timeout_ms: u64,
    cancel: &dyn Cancel,
    emit: &dyn Emit,
) -> Output {
    let job_cancel = JobCancel::new();
    match jobs.open(Opening {
        tool: "shell".to_owned(),
        description: description_of(command),
        stop: job_cancel.stop(),
    }) {
        Ok(opened) => hand_off(moved, opened, job_cancel, clock, timeout_ms),
        Err(error) => {
            let finished = moved.resume(clock.as_ref(), cancel, emit);
            note_open_failure(assemble(timeout_ms, finished), &error)
        }
    }
}

fn hand_off(
    mut moved: Moved,
    opened: Opened,
    job_cancel: Arc<JobCancel>,
    clock: Arc<dyn Clock>,
    timeout_ms: u64,
) -> Output {
    let pgid = moved.pgid();
    let reason = moved.reason.clone();
    moved.attach_output(opened.file);
    // Armed before the call's cancel is detached, so a stop that arrives
    // as soon as the job is recorded still wakes the drive loop.
    moved.arm(job_cancel.as_ref());
    moved.detach_call_cancel();
    let path = opened.path.clone();
    let job_id = opened.started.job_id.clone();
    let end = opened.end;
    let job_clock = Arc::clone(&clock);
    // The job's thread starts before the receipt lists the group, so its
    // timeout and stop hold while `ps` runs.
    thread::spawn(move || {
        let finished = moved.drive_job(job_clock.as_ref(), job_cancel.as_ref());
        end.end(to_completed(job_id, &path, finished, timeout_ms));
    });
    let members = match reason {
        MoveReason::ShellExited { .. } => group_members(Path::new("ps"), pgid, clock.as_ref()),
        MoveReason::AfterThirtySeconds | MoveReason::StartedInBackground => None,
    };
    receipt(&opened.started, &opened.path, &reason, members.as_deref())
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
    let Some(stdout) = until_listed(&rx, clock, clock.now().checked_add(PS_BOUND)) else {
        // Std's `Child::kill` signals this `ps` alone, never a group.
        let _killed = child.kill();
        let _reaped = child.wait();
        return None;
    };
    // The pipe closed, so `ps` is exiting.
    if !child.wait().is_ok_and(|status| status.success()) {
        return None;
    }
    let rows = parse_ps(&String::from_utf8_lossy(&stdout), pgid);
    (!rows.is_empty()).then(|| format_rows(&rows))
}

/// `Some(bytes)` is `ps`'s whole output; `None` says the clock moved.
type Listed = Option<Vec<u8>>;

/// Wakes [`until_listed`] when the clock moves.
struct Tick(mpsc::Sender<Listed>);

impl Wake for Tick {
    fn wake(&self) {
        let _sent = self.0.send(None);
    }
}

/// `ps`'s output, or `None` once `deadline` passes on `clock`. A clock at or
/// past the deadline hands the wait a zero bound, which takes only output
/// already sent.
fn until_listed(
    rx: &mpsc::Receiver<Listed>,
    clock: &dyn Clock,
    deadline: Option<Instant>,
) -> Listed {
    loop {
        let mut received = None;
        clock.wait_until(deadline, &mut |bound| {
            received = Some(match bound {
                Some(bound) => rx.recv_timeout(bound).ok(),
                None => rx.recv().ok(),
            });
        });
        match received {
            Some(Some(Some(bytes))) => return Some(bytes),
            Some(Some(None)) => {}
            Some(None) | None => return None,
        }
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
fn to_completed(job_id: JobId, path: &Path, finished: Finished, timeout_ms: u64) -> JobCompleted {
    let stopped = finished.stop == Some(StopKind::Cancel) && !finished.indeterminate;
    let output = assemble(timeout_ms, finished);
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
