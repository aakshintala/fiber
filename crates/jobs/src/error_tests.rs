//! One row per [`JobsError`](super::JobsError) case: its code and its
//! sentence. Two rows per case whose text branches, so a changed code
//! or a changed word fails here.

use contract::ErrorCode;
use contract::JobId;
use contract::shapes::Failure;

use super::JobsError;

struct Row {
    name: &'static str,
    error: JobsError,
    code: ErrorCode,
    message: &'static str,
}

fn job(id: &str) -> JobId {
    JobId(id.to_owned())
}

fn io_source(why: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotFound, why.to_owned())
}

fn refused(code: ErrorCode, message: &str) -> JobsError {
    JobsError::SteerRefused {
        id: job("j_steer"),
        failure: Failure {
            code,
            message: message.to_owned(),
            retry_after_ms: None,
            provider: None,
        },
    }
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            name: "unknown isolation",
            error: JobsError::IsolationUnknown,
            code: ErrorCode::InvalidArguments,
            message: "`isolation` must be `none` or `worktree`.",
        },
        Row {
            name: "worktree creation",
            error: JobsError::DelegateWorktree {
                failure: Failure {
                    code: ErrorCode::IoFailed,
                    message: "git worktree add failed.".into(),
                    retry_after_ms: None,
                    provider: None,
                },
            },
            code: ErrorCode::IoFailed,
            message: "Starting the delegate failed: git worktree add failed.",
        },
        Row {
            name: "worktree outside repository",
            error: JobsError::DelegateWorktree {
                failure: Failure {
                    code: ErrorCode::InvalidArguments,
                    message: "not a repository.".into(),
                    retry_after_ms: None,
                    provider: None,
                },
            },
            code: ErrorCode::InvalidArguments,
            message: "Starting the delegate failed: not a repository.",
        },
        Row {
            name: "arguments missing, one",
            error: JobsError::ArgumentsMissing {
                names: vec!["description".to_owned()],
            },
            code: ErrorCode::InvalidArguments,
            message: "Give description as a string.",
        },
        Row {
            name: "arguments missing, two",
            error: JobsError::ArgumentsMissing {
                names: vec!["description".to_owned(), "prompt".to_owned()],
            },
            code: ErrorCode::InvalidArguments,
            message: "Give description, prompt as strings.",
        },
        Row {
            name: "arguments not strings, one",
            error: JobsError::ArgumentsNotStrings {
                names: vec!["prompt".to_owned()],
            },
            code: ErrorCode::InvalidArguments,
            message: "prompt must be a string.",
        },
        Row {
            name: "arguments not strings, two",
            error: JobsError::ArgumentsNotStrings {
                names: vec!["id".to_owned(), "message".to_owned()],
            },
            code: ErrorCode::InvalidArguments,
            message: "id, message must be strings.",
        },
        Row {
            name: "arguments not allowed, one",
            error: JobsError::ArgumentsNotAllowed {
                keys: vec!["model".to_owned()],
            },
            code: ErrorCode::InvalidArguments,
            message: "`/model`: not allowed",
        },
        Row {
            name: "arguments not allowed, two",
            error: JobsError::ArgumentsNotAllowed {
                keys: vec!["model".to_owned(), "extra".to_owned()],
            },
            code: ErrorCode::InvalidArguments,
            message: "`/model`: not allowed\n`/extra`: not allowed",
        },
        Row {
            name: "timeout not positive",
            error: JobsError::TimeoutNotPositive,
            code: ErrorCode::InvalidArguments,
            message: "`timeout_ms` must be an integer of 1 or more.",
        },
        Row {
            name: "model missing",
            error: JobsError::ModelMissing {
                catalogue: "roles: code".to_owned(),
            },
            code: ErrorCode::InvalidArguments,
            message: "Give model: a role or a full reference.\nroles: code",
        },
        Row {
            name: "model not string",
            error: JobsError::ModelNotString {
                catalogue: "roles: code".to_owned(),
            },
            code: ErrorCode::InvalidArguments,
            message: "model must be a string.\nroles: code",
        },
        Row {
            name: "model unresolved",
            error: JobsError::ModelUnresolved {
                reason: "Unknown role `nope`.".to_owned(),
                catalogue: "roles: code".to_owned(),
            },
            code: ErrorCode::InvalidArguments,
            message: "Unknown role `nope`.\nroles: code",
        },
        Row {
            name: "credential unavailable",
            error: JobsError::CredentialUnavailable {
                message: "No credential for label `x`.".to_owned(),
            },
            code: ErrorCode::CredentialMissing,
            message: "No credential for label `x`.",
        },
        Row {
            name: "depth exceeded, spawn",
            error: JobsError::DepthExceeded {
                tool: "delegate_spawn".to_owned(),
            },
            code: ErrorCode::DepthExceeded,
            message: "This delegate is at depth 2, the deepest a delegate can be, so it cannot use `delegate_spawn`; do the work in this session.",
        },
        Row {
            name: "depth exceeded, message",
            error: JobsError::DepthExceeded {
                tool: "delegate_message".to_owned(),
            },
            code: ErrorCode::DepthExceeded,
            message: "This delegate is at depth 2, the deepest a delegate can be, so it cannot use `delegate_message`; do the work in this session.",
        },
        Row {
            name: "fork point",
            error: JobsError::ForkPoint {
                failure: Failure {
                    code: ErrorCode::InvalidArguments,
                    message: "no forkable point".to_owned(),
                    retry_after_ms: None,
                    provider: None,
                },
            },
            code: ErrorCode::InvalidArguments,
            message: "Starting the fork failed: no forkable point",
        },
        Row {
            name: "no inbox",
            error: JobsError::NoInbox,
            code: ErrorCode::ToolError,
            message: "No session is listening for this delegate's end, so nothing was started.",
        },
        Row {
            name: "spawn failed",
            error: JobsError::SpawnFailed {
                source: io_source("denied"),
            },
            code: ErrorCode::ToolError,
            message: "Starting the delegate failed: denied.",
        },
        Row {
            name: "delegates full",
            error: JobsError::DelegatesFull,
            code: ErrorCode::ToolError,
            message: "This session already runs 10 delegates, the most it can run at once, so nothing was started; wait for one with `jobs wait` or stop one with `jobs stop`, then try again.",
        },
        Row {
            name: "extension starts closed",
            error: JobsError::ExtensionStartsClosed,
            code: ErrorCode::Closing,
            message: "The session is ending, so no delegate was started.",
        },
        Row {
            name: "message empty",
            error: JobsError::MessageEmpty,
            code: ErrorCode::InvalidArguments,
            message: "`message` must not be empty.",
        },
        Row {
            name: "unknown job",
            error: JobsError::UnknownJob { id: job("j_gone") },
            code: ErrorCode::InvalidArguments,
            message: "No job has id `j_gone`.",
        },
        Row {
            name: "not delegate",
            error: JobsError::NotDelegate { id: job("j_shell") },
            code: ErrorCode::InvalidArguments,
            message: "Job `j_shell` is not a delegate.",
        },
        Row {
            name: "steer closing",
            error: JobsError::SteerClosing { id: job("j_live") },
            code: ErrorCode::Closing,
            message: "Delegate j_live is finishing its run and did not take the message; send it again once it has finished.",
        },
        Row {
            name: "steer refused forwards message refused",
            error: refused(ErrorCode::MessageRefused, "too long"),
            code: ErrorCode::MessageRefused,
            message: "Delegate j_steer refused the message: too long",
        },
        Row {
            name: "steer refused forwards hook failed",
            error: refused(ErrorCode::HookFailed, "hook said no"),
            code: ErrorCode::HookFailed,
            message: "Delegate j_steer refused the message: hook said no",
        },
        Row {
            name: "steer refused other",
            error: JobsError::SteerRefusedOther {
                id: job("j_live"),
                delegate_code: "busy".to_owned(),
                message: "try later".to_owned(),
            },
            code: ErrorCode::ToolError,
            message: "Delegate j_live refused the message (busy): try later",
        },
        Row {
            name: "answer lost",
            error: JobsError::AnswerLost { id: job("j_live") },
            code: ErrorCode::Indeterminate,
            message: "Delegate j_live received the message, but its answer was lost; it joins the delegate's turn unless the delegate refused it.",
        },
        Row {
            name: "unanswered",
            error: JobsError::Unanswered { id: job("j_live") },
            code: ErrorCode::Indeterminate,
            message: "Delegate j_live ended before answering; the message may have joined its last turn. Send it again to resume the delegate.",
        },
        Row {
            name: "resume exhausted",
            error: JobsError::ResumeExhausted { id: job("j_live") },
            code: ErrorCode::ToolError,
            message: "Delegate j_live cannot run again: its run count is exhausted.",
        },
        Row {
            name: "resume boundary",
            error: JobsError::ResumeBoundary {
                id: job("j_live"),
                reason: "/tmp/x.log: missing".to_owned(),
            },
            code: ErrorCode::ToolError,
            message: "Resuming delegate j_live failed: its log could not be read: /tmp/x.log: missing. Nothing was started.",
        },
        Row {
            name: "resume spawn",
            error: JobsError::ResumeSpawn {
                source: io_source("denied"),
            },
            code: ErrorCode::ToolError,
            message: "Resuming the delegate failed: denied.",
        },
        Row {
            name: "action unknown",
            error: JobsError::ActionUnknown,
            code: ErrorCode::InvalidArguments,
            message: "`action` must be `list`, `wait`, `write` or `stop`.",
        },
        Row {
            name: "job id not string",
            error: JobsError::JobIdNotString,
            code: ErrorCode::InvalidArguments,
            message: "`job_id` must be a string.",
        },
        Row {
            name: "job id missing",
            error: JobsError::JobIdMissing,
            code: ErrorCode::InvalidArguments,
            message: "Give the job as `job_id`.",
        },
        Row {
            name: "timeout missing",
            error: JobsError::TimeoutMissing,
            code: ErrorCode::InvalidArguments,
            message: "Give how long to wait as `timeout_ms`.",
        },
        Row {
            name: "timeout not integer",
            error: JobsError::TimeoutNotInteger,
            code: ErrorCode::InvalidArguments,
            message: "`timeout_ms` must be an integer of 0 or more.",
        },
        Row {
            name: "input not string",
            error: JobsError::InputNotString,
            code: ErrorCode::InvalidArguments,
            message: "`input` must be a string.",
        },
        Row {
            name: "input missing",
            error: JobsError::InputMissing,
            code: ErrorCode::InvalidArguments,
            message: "Give what to type as `input`.",
        },
        Row {
            name: "write wait too long",
            error: JobsError::WriteWaitTooLong { max: 30000 },
            code: ErrorCode::InvalidArguments,
            message: "`timeout_ms` for `write` is at most 30000.",
        },
        Row {
            name: "not tty",
            error: JobsError::NotTty { id: job("j_1") },
            code: ErrorCode::InvalidArguments,
            message: "Job j_1 was not started with `tty`.",
        },
        Row {
            name: "job ended",
            error: JobsError::JobEnded {
                id: job("j_1"),
                status: "failed",
            },
            code: ErrorCode::InvalidArguments,
            message: "Job `j_1` has ended: failed.",
        },
        Row {
            name: "write failed",
            error: JobsError::WriteFailed {
                id: job("j_1"),
                source: io_source("closed"),
            },
            code: ErrorCode::ToolError,
            message: "Writing to job j_1 failed: closed.",
        },
        Row {
            name: "delegate timed out",
            error: JobsError::DelegateTimedOut,
            code: ErrorCode::Timeout,
            message: "The delegate passed its `timeout_ms` and was stopped.",
        },
        Row {
            name: "delegate output cap",
            error: JobsError::DelegateOutputCap,
            code: ErrorCode::OutputCap,
            message: "The delegate's log passed the output cap and the job was stopped.",
        },
        Row {
            name: "delegate killed",
            error: JobsError::DelegateKilled {
                signal: "SIGTERM".to_owned(),
            },
            code: ErrorCode::Signal,
            message: "Killed by SIGTERM.",
        },
        Row {
            name: "delegate no result",
            error: JobsError::DelegateNoResult,
            code: ErrorCode::Indeterminate,
            message: "The delegate ended without a result.",
        },
        Row {
            name: "delegate exit code",
            error: JobsError::DelegateExitCode { code: 1 },
            code: ErrorCode::NonzeroExit,
            message: "Exit code 1.",
        },
        Row {
            name: "job no result",
            error: JobsError::JobNoResult,
            code: ErrorCode::Indeterminate,
            message: "The job ended without a result.",
        },
    ]
}

#[test]
fn each_case_maps_to_its_code_sentence_and_record() {
    for row in rows() {
        assert_eq!(row.error.code(), row.code, "code, row: {}", row.name);
        assert_eq!(
            row.error.to_string(),
            row.message,
            "message, row: {}",
            row.name
        );
        let record = Failure::from(&row.error);
        assert_eq!(record.code, row.code, "record code, row: {}", row.name);
        assert_eq!(
            record.message, row.message,
            "record message, row: {}",
            row.name
        );
        assert_eq!(
            record.retry_after_ms, None,
            "record retry, row: {}",
            row.name
        );
        assert_eq!(record.provider, None, "record provider, row: {}", row.name);
    }
}

#[test]
fn output_differs_only_by_the_trailing_newline() {
    for row in rows() {
        let plain = row.error.output(false);
        let lined = row.error.output(true);
        assert_eq!(plain.content.len(), 1, "one part, row: {}", row.name);
        assert_eq!(lined.content.len(), 1, "one part, row: {}", row.name);
        let (plain_text, lined_text) = match (&plain.content[0], &lined.content[0]) {
            (
                contract::shapes::ContentPart::Text { text: plain },
                contract::shapes::ContentPart::Text { text: lined },
            ) => (plain.clone(), lined.clone()),
            _ => panic!("text content, row: {}", row.name),
        };
        assert_eq!(plain_text, row.message, "plain text, row: {}", row.name);
        assert_eq!(
            lined_text,
            format!("{}\n", row.message),
            "lined text, row: {}",
            row.name
        );
        let plain_failure = plain.error.expect("plain output errors");
        let lined_failure = lined.error.expect("lined output errors");
        assert_eq!(plain_failure, lined_failure, "error, row: {}", row.name);
        assert_eq!(lined_failure.code, row.code, "code, row: {}", row.name);
        assert_eq!(
            lined_failure.message, row.message,
            "message, row: {}",
            row.name
        );
    }
}
