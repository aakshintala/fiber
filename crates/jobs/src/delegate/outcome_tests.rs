//! Tests first for the outcome fold: each termination reason over the
//! other rows, and the socket line winning over the drain's.

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use contract::events::{FiberExited, FinalMessage, Outcome};
use contract::shapes::{Failure, Question, Tokens, Usage};
use contract::{ActionId, ErrorCode, JobId};

use super::{Termination, outcome};

fn job_id() -> JobId {
    JobId("j_0123456789abcdef".into())
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 5,
        },
        cost: Some(0.25),
        subscription_cost: 0.0,
    }
}

fn exited(text: &str) -> FiberExited {
    FiberExited {
        exit_code: 0,
        usage: usage(),
        final_message: Some(FinalMessage {
            final_action_id: ActionId("a_1".into()),
            text: text.to_owned(),
        }),
        error: None,
        suspended_on: None,
        questions: None,
    }
}

fn errored() -> FiberExited {
    FiberExited {
        exit_code: 1,
        usage: usage(),
        final_message: Some(FinalMessage {
            final_action_id: ActionId("a_1".into()),
            text: "almost".to_owned(),
        }),
        error: Some(Failure {
            code: ErrorCode::InvalidArguments,
            message: "Bad model.".to_owned(),
            retry_after_ms: None,
            provider: None,
        }),
        suspended_on: None,
        questions: None,
    }
}

fn exit(code: i32) -> ExitStatus {
    ExitStatus::from_raw(code << 8)
}

fn killed(signal: i32) -> ExitStatus {
    ExitStatus::from_raw(signal)
}

#[test]
fn termination_beats_the_exit() {
    // One row per former test: a_stop_beats_an_error_exit,
    // a_stop_beats_a_clean_exit_zero, the_cap_beats_an_error_exit and
    // the_cap_beats_a_clean_exit_zero. The termination reason wins over
    // whatever the exit carried.
    for (termination, clean, status, expected, code) in [
        (Termination::Stopped, false, exit(1), Outcome::Cancelled, None),
        (Termination::Stopped, true, exit(0), Outcome::Cancelled, None),
        (
            Termination::OutputCap,
            false,
            exit(1),
            Outcome::Failed,
            Some(ErrorCode::OutputCap),
        ),
        (
            Termination::OutputCap,
            true,
            exit(0),
            Outcome::Failed,
            Some(ErrorCode::OutputCap),
        ),
    ] {
        let line = if clean { exited("Done.") } else { errored() };
        let (completed, finished) =
            outcome(Some(termination), Some(&line), None, status, &job_id());
        assert_eq!(completed.status, expected, "{termination:?} clean={clean}");
        assert_eq!(
            completed.error.as_ref().map(|error| error.code.clone()),
            code,
            "{termination:?} clean={clean}"
        );
        if matches!(termination, Termination::Stopped) && clean {
            assert_eq!(finished.text, "Done.");
        }
    }
}

#[test]
fn an_error_exit_keeps_its_error() {
    let errored = errored();
    let (completed, finished) = outcome(None, Some(&errored), None, exit(1), &job_id());
    assert_eq!(completed.status, Outcome::Failed);
    assert_eq!(
        completed.error.as_ref().map(|error| error.message.clone()),
        Some("Bad model.".to_owned())
    );
    assert_eq!(
        completed.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(finished.text, "almost");
}

#[test]
fn an_error_beats_a_signal_after_the_line() {
    let errored = errored();
    let (completed, _) = outcome(None, Some(&errored), None, killed(9), &job_id());
    assert_eq!(completed.status, Outcome::Failed);
    assert_eq!(
        completed.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::InvalidArguments)
    );
}

#[test]
fn a_signal_with_no_line_is_signal() {
    let (completed, finished) = outcome(None, None, None, killed(9), &job_id());
    assert_eq!(completed.status, Outcome::Failed);
    assert_eq!(
        completed.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::Signal)
    );
    assert_eq!(
        completed
            .process
            .as_ref()
            .and_then(|process| process.signal.clone()),
        Some("SIGKILL".to_owned())
    );
    assert_eq!(finished.text, "");
}

#[test]
fn an_exit_with_no_line_is_indeterminate() {
    // One row per former test: exit_zero_with_no_line_is_indeterminate
    // and exit_one_with_no_line_is_indeterminate. Not `signal`: no
    // signal ended the process. Not `nonzero_exit`: with no
    // `fiber_exited` there is no run to blame the code on.
    for code in [0, 1] {
        let (completed, finished) = outcome(None, None, None, exit(code), &job_id());
        assert_eq!(completed.status, Outcome::Failed, "exit {code}");
        assert_eq!(
            completed.error.as_ref().map(|error| error.code.clone()),
            Some(ErrorCode::Indeterminate),
            "exit {code}"
        );
        assert_eq!(finished.text, "");
    }
}

#[test]
fn a_clean_exit_zero_completes() {
    let clean = exited("Done.");
    let (completed, finished) = outcome(None, Some(&clean), None, exit(0), &job_id());
    assert_eq!(completed.status, Outcome::Completed);
    assert_eq!(completed.error, None);
    assert_eq!(
        completed
            .process
            .as_ref()
            .and_then(|process| process.exit_code),
        Some(0)
    );
    assert_eq!(finished.text, "Done.");
    assert_eq!(finished.job_id, job_id());
}

#[test]
fn a_clean_exit_one_is_nonzero_exit() {
    let clean = exited("Done.");
    let (completed, _) = outcome(None, Some(&clean), None, exit(1), &job_id());
    assert_eq!(completed.status, Outcome::Failed);
    assert_eq!(
        completed.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::NonzeroExit)
    );
}

#[test]
fn a_signal_after_the_line_is_signal() {
    let clean = exited("Done.");
    let (completed, finished) = outcome(None, Some(&clean), None, killed(15), &job_id());
    assert_eq!(completed.status, Outcome::Failed);
    assert_eq!(
        completed.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::Signal)
    );
    assert_eq!(finished.text, "Done.");
}

#[test]
fn the_socket_line_wins_over_stdout() {
    let socket = exited("socket");
    let stdout = exited("stdout");
    let (_, finished) = outcome(None, Some(&socket), Some(&stdout), exit(0), &job_id());
    assert_eq!(finished.text, "socket");
}

#[test]
fn stdout_is_used_when_the_socket_gave_nothing() {
    let stdout = exited("stdout");
    let (completed, finished) = outcome(None, None, Some(&stdout), exit(0), &job_id());
    assert_eq!(completed.status, Outcome::Completed);
    assert_eq!(finished.text, "stdout");
}

#[test]
fn the_finish_carries_questions_and_usage() {
    let mut clean = exited("Done.");
    clean.questions = Some(vec![Question {
        header: "Base".to_owned(),
        question: "Which branch?".to_owned(),
        options: Vec::new(),
        multi_select: None,
    }]);
    let (_, finished) = outcome(None, Some(&clean), None, exit(0), &job_id());
    assert_eq!(finished.usage, usage());
    assert!(finished.questions.is_some());
    assert_eq!(finished.artifact, None);
}

#[test]
fn no_line_means_empty_text_and_zero_usage() {
    let (_, finished) = outcome(None, None, None, exit(0), &job_id());
    assert_eq!(finished.text, "");
    assert_eq!(finished.job_id, job_id());
    assert_eq!(finished.usage.tokens.input, 0);
    assert_eq!(finished.usage.tokens.output, 0);
    assert_eq!(finished.usage.cost, Some(0.0));
    assert_eq!(finished.questions, None);
}

#[test]
fn a_delegate_killed_by_sigbus_reports_sigbus() {
    // SIGBUS's number moves by platform, so it is built from the signal
    // itself; the second row pins the fallback for a number the shared
    // table names nothing. Each must name itself, or the fallback names
    // it.
    for (number, name) in [
        (rustix::process::Signal::BUS.as_raw(), "SIGBUS"),
        (29, "SIG29"),
    ] {
        let (completed, _) = outcome(None, None, None, killed(number), &job_id());
        assert_eq!(completed.status, Outcome::Failed);
        assert_eq!(
            completed.error.as_ref().map(|error| error.code.clone()),
            Some(ErrorCode::Signal),
            "signal {number}"
        );
        assert_eq!(
            completed.error.as_ref().map(|error| error.message.clone()),
            Some(format!("Killed by {name}.")),
            "signal {number}"
        );
        assert_eq!(
            completed
                .process
                .as_ref()
                .and_then(|process| process.signal.clone()),
            Some(name.to_owned()),
            "signal {number}"
        );
    }
}
