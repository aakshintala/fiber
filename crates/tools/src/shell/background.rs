//! Moving a running command onto a job (`docs/tools.md`, "Moving to the background").
//! The call thread opens the job and returns the receipt. The job's thread
//! runs the command to the end.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::Instant;

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{JobCompleted, JobStarted, Outcome};
use contract::jobs::{JobRecord, Jobs, OpenError, Opened, Opening, Stop};
use contract::shapes::{ContentPart, Failure, Process};
use contract::tool::{Cancel, Output};
use contract::{ErrorCode, JobId};

use super::command::{Finished, MovePolicy, MoveReason, Moved, StopKind};
use super::{INDETERMINATE, assemble, failure, observed, process_of, timeout_line};

/// A description longer than this is cut, with `…` as the last character.
/// Picked, not measured.
const DESCRIPTION_LIMIT: usize = 80;

/// `output_tail` keeps this many bytes. Picked, not measured.
const TAIL_BYTES: usize = 2_048;

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
    Move(MoveKind),
    /// Wait for the next wake.
    Park,
}

/// Which move won.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MoveKind {
    /// The shell exited and left members in the group.
    ShellExited,
    /// `run_in_background`.
    Background,
    /// 30 seconds have passed.
    AfterThirtySeconds,
}

pub(crate) fn running_step(
    policy: MovePolicy,
    timed_out: bool,
    cancelled: bool,
    seen_empty: bool,
    reaped: bool,
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
            if reaped {
                Step::Move(MoveKind::ShellExited)
            } else if policy == MovePolicy::Background {
                Step::Move(MoveKind::Background)
            } else if move_due {
                Step::Move(MoveKind::AfterThirtySeconds)
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
    let receipt = receipt(&opened.started, &opened.path, &reason, pgid);
    let path = opened.path.clone();
    let job_id = opened.started.job_id.clone();
    let end = opened.end;
    thread::spawn(move || {
        let finished = moved.drive_job(clock.as_ref(), job_cancel.as_ref());
        end.end(to_completed(job_id, &path, &finished, timeout_ms));
    });
    receipt
}

fn note_open_failure(mut output: Output, error: &OpenError) -> Output {
    let line = format!("It could not move to the background: {error}.");
    match output.content.first_mut() {
        Some(ContentPart::Text { text }) => {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&line);
            text.push('\n');
        }
        Some(ContentPart::Image { .. }) | Some(ContentPart::Unknown) | None => {
            output.content.push(ContentPart::Text {
                text: format!("{line}\n"),
            });
        }
    }
    output
}

fn receipt(started: &JobStarted, path: &Path, reason: &MoveReason, pgid: u32) -> Output {
    let why = sentence(reason, pgid);
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

fn sentence(reason: &MoveReason, pgid: u32) -> String {
    match reason {
        MoveReason::AfterThirtySeconds => AFTER_THIRTY.to_owned(),
        MoveReason::StartedInBackground => STARTED.to_owned(),
        MoveReason::ShellExited { code } => shell_sentence(*code, group_members(pgid).as_deref()),
    }
}

fn shell_sentence(code: i32, members: Option<&str>) -> String {
    match members {
        Some(names) if !names.is_empty() => format!(
            "The shell exited with code {code}, leaving {names} running, so it moved to the background."
        ),
        Some(_) | None => {
            format!("The shell exited with code {code}, so it moved to the background. {UNNAMED}")
        }
    }
}

/// `Some` when `ps` ran: the formatted names, empty when the group had no
/// rows. `None` when `ps` could not be run.
fn group_members(pgid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-A", "-o", "pid=,pgid=,comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let rows = parse_ps(&text, pgid);
    if rows.is_empty() {
        Some(String::new())
    } else {
        Some(format_rows(&rows))
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

fn to_completed(job_id: JobId, path: &Path, finished: &Finished, timeout_ms: u64) -> JobCompleted {
    let (status, error, process) = classify(finished, timeout_ms);
    JobCompleted {
        job_id,
        status,
        error,
        process: Some(process),
        output_tail: match status {
            Outcome::Failed => output_tail(path),
            Outcome::Completed | Outcome::Cancelled => None,
        },
    }
}

fn classify(finished: &Finished, timeout_ms: u64) -> (Outcome, Option<Failure>, Process) {
    let timed_out = finished.stop == Some(StopKind::Timeout);
    if finished.indeterminate {
        return (
            Outcome::Failed,
            Some(failure(ErrorCode::Indeterminate, INDETERMINATE.to_owned())),
            process_of(finished, timed_out),
        );
    }
    if finished.stop == Some(StopKind::Timeout) {
        let line = timeout_line(timeout_ms);
        return (
            Outcome::Failed,
            Some(failure(ErrorCode::Timeout, line)),
            process_of(finished, true),
        );
    }
    if finished.stop == Some(StopKind::Cancel) {
        return (Outcome::Cancelled, None, process_of(finished, false));
    }
    let Some(status) = finished.status else {
        return (
            Outcome::Failed,
            Some(failure(
                ErrorCode::ToolError,
                "The command's process could not be reaped.".to_owned(),
            )),
            process_of(finished, false),
        );
    };
    let (exit_code, signal, line) = observed(status);
    let error = if signal.is_some() && !finished.sent_signal {
        Some(failure(ErrorCode::Signal, line))
    } else if exit_code.is_some_and(|code| code != 0) {
        Some(failure(ErrorCode::NonzeroExit, line))
    } else {
        None
    };
    let outcome = if error.is_some() {
        Outcome::Failed
    } else {
        Outcome::Completed
    };
    (
        outcome,
        error,
        Process {
            exit_code,
            signal,
            timed_out: false,
        },
    )
}

fn output_tail(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let start = bytes.len().saturating_sub(TAIL_BYTES);
    let slice = bytes.get(start..).unwrap_or(&bytes);
    let text = String::from_utf8_lossy(align_char_boundary(slice)).into_owned();
    if text.is_empty() { None } else { Some(text) }
}

/// Drops a partial character at the start of a cut, so the tail does not
/// begin with U+FFFD from a byte that belonged to the previous character.
fn align_char_boundary(slice: &[u8]) -> &[u8] {
    let mut index = 0;
    while slice
        .get(index)
        .is_some_and(|byte| byte & 0b1100_0000 == 0b1000_0000)
    {
        index += 1;
    }
    slice.get(index..).unwrap_or(b"")
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
