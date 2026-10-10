//! How a Fiber delegate's job ends: a pure fold over the termination
//! reason the runner itself set, the `fiber_exited` the socket received
//! or the drain kept, and the exit status, checked in that order
//! (`docs/delegates.md`, "Lifetime").

use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use contract::events::{DelegateFinished, FiberExited, JobCompleted, Outcome};
use contract::shapes::{Failure, Process, Usage};
use contract::{ErrorCode, JobId};

/// Why the runner itself ended the delegate. Set once, under the runner's
/// lock, before the runner's first signal: a delegate the runner ended
/// therefore ends `cancelled` or `failed` `output_cap`, never `failed`
/// `signal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Termination {
    /// A `jobs stop`, a `job_stop`, a budget end or shutdown.
    Stopped,
    /// The delegate's `events.jsonl` passed the output cap.
    OutputCap,
}

/// The pure fold. `socket` is the subscription's
/// `fiber_exited` when it received one; `stdout` is the drain's last line,
/// used only then (`docs/delegates.md`, "Streams").
pub(crate) fn outcome(
    termination: Option<Termination>,
    socket: Option<&FiberExited>,
    stdout: Option<&FiberExited>,
    status: ExitStatus,
    job_id: &JobId,
) -> (JobCompleted, DelegateFinished) {
    // The socket's line is used when the subscription received one; the
    // stdout line only when it did not.
    let exited = socket.or(stdout);
    let process = process_of(status);
    let finished = match exited {
        Some(exited) => DelegateFinished {
            job_id: job_id.clone(),
            text: exited
                .final_message
                .as_ref()
                .map(|message| message.text.clone())
                .unwrap_or_default(),
            artifact: None,
            questions: exited.questions.clone(),
            usage: exited.usage.clone(),
            worktree: None,
        },
        None => empty(job_id),
    };
    let completed = match termination {
        Some(Termination::Stopped) => JobCompleted {
            job_id: job_id.clone(),
            status: Outcome::Cancelled,
            error: None,
            process: Some(process),
            output_tail: None,
        },
        Some(Termination::OutputCap) => failed(
            job_id,
            ErrorCode::OutputCap,
            "The delegate's log passed the output cap and the job was stopped.",
            process,
        ),
        None => match exited.and_then(|exited| exited.error.clone()) {
            Some(error) => JobCompleted {
                job_id: job_id.clone(),
                status: Outcome::Failed,
                error: Some(error),
                process: Some(process),
                output_tail: None,
            },
            None => without_error(job_id, exited, status, process),
        },
    };
    (completed, finished)
}

/// The fold once termination and the error line are out of the way.
/// A signal ends the job `signal`, whether or not a `fiber_exited` was
/// seen; without one there is no run to blame any other status on.
fn without_error(
    job_id: &JobId,
    exited: Option<&FiberExited>,
    status: ExitStatus,
    process: Process,
) -> JobCompleted {
    // Killed by a signal: with no `fiber_exited`, or after writing one,
    // for example by an outside SIGTERM.
    if let Some(number) = status.signal() {
        let name = support::group::signal_name(number);
        return failed(
            job_id,
            ErrorCode::Signal,
            &format!("Killed by {name}."),
            process,
        );
    }
    // No `fiber_exited` was seen otherwise, exit 0 included: there is no
    // run to blame the status on.
    if exited.is_none() {
        return failed(
            job_id,
            ErrorCode::Indeterminate,
            "The delegate ended without a result.",
            process,
        );
    }
    match status.code() {
        Some(0) => JobCompleted {
            job_id: job_id.clone(),
            status: Outcome::Completed,
            error: None,
            process: Some(process),
            output_tail: None,
        },
        _ => failed(
            job_id,
            ErrorCode::NonzeroExit,
            &format!("Exit code {}.", status.code().unwrap_or(0)),
            process,
        ),
    }
}

fn failed(job_id: &JobId, code: ErrorCode, message: &str, process: Process) -> JobCompleted {
    JobCompleted {
        job_id: job_id.clone(),
        status: Outcome::Failed,
        error: Some(Failure {
            code,
            message: message.to_owned(),
            retry_after_ms: None,
            provider: None,
        }),
        process: Some(process),
        output_tail: None,
    }
}

/// `process`, filled as for shell jobs: the exit code, or the signal's
/// name when a signal ended the process.
fn process_of(status: ExitStatus) -> Process {
    Process {
        exit_code: status.code(),
        signal: status.signal().map(support::group::signal_name),
        timed_out: false,
    }
}

/// A delegate that ended without reporting one: empty text, zero usage.
/// Both the outcome fold's no-line case and the registry's dropped report
/// build it from here, so the two never drift apart.
pub(crate) fn empty(job_id: &JobId) -> DelegateFinished {
    DelegateFinished {
        job_id: job_id.clone(),
        text: String::new(),
        artifact: None,
        questions: None,
        usage: Usage::default(),
        worktree: None,
    }
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
