//! How a Fiber delegate's job ends: a pure fold over the termination
//! reason the runner itself set, the one `fiber_exited` ruling 9 chose,
//! and the exit status, checked in ruling 10's order
//! (`docs/delegates.md`, "Lifetime").

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use contract::events::{DelegateFinished, FiberExited, JobCompleted, Outcome};
use contract::shapes::{Failure, Process, Tokens, Usage};
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

/// The pure fold from ruling 10. `socket` is the subscription's
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
    let finished = DelegateFinished {
        job_id: job_id.clone(),
        text: exited
            .and_then(|exited| exited.final_message.as_ref())
            .map(|message| message.text.clone())
            .unwrap_or_default(),
        artifact: None,
        questions: exited.and_then(|exited| exited.questions.clone()),
        usage: exited
            .map(|exited| exited.usage.clone())
            .unwrap_or_else(zero_usage),
        worktree: None,
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

/// The fold once termination and the error line are out of the way: a
/// missing `fiber_exited` fails the job, whatever the status was.
fn without_error(
    job_id: &JobId,
    exited: Option<&FiberExited>,
    status: ExitStatus,
    process: Process,
) -> JobCompleted {
    // No `fiber_exited` was seen and the process was killed by a signal.
    if exited.is_none() && status.signal().is_some() {
        let name = signal_name(status.signal().unwrap_or(0));
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
    if status.signal().is_some() {
        // Killed by a signal after writing `fiber_exited`, for example by
        // an outside SIGTERM.
        let name = signal_name(status.signal().unwrap_or(0));
        return failed(
            job_id,
            ErrorCode::Signal,
            &format!("Killed by {name}."),
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
        signal: status.signal().map(signal_name),
        timed_out: false,
    }
}

/// The signal's name, as shell jobs report it.
fn signal_name(number: i32) -> String {
    match number {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => return format!("SIG{number}"),
    }
    .to_owned()
}

/// No `fiber_exited` was seen, so no model call is known: zero.
fn zero_usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        },
        cost: Some(0.0),
        subscription_cost: 0.0,
    }
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
