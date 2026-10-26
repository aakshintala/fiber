//! Every `jobs` failure as its own case (`docs/errors.md`, "Where a code
//! comes from"): one fixed code per case, mapped in [`JobsError::code`]
//! with no wildcard arm, so a new case does not compile until it has one.
//! Each case's message is the sentence shown for it, and the contract
//! records (`Output.error`) are built from a case at the edge through
//! `From<&JobsError> for Failure`. Each tool builds a case of this enum
//! and converts to `Failure` once at the `Output` edge.

use std::io;

use contract::ErrorCode;
use contract::JobId;
use contract::shapes::{ContentPart, Failure};
use contract::tool::Output;

/// Every way a `jobs` tool call, a delegate start, a steer or a resume
/// fails. A function that crosses a crate boundary returns this enum,
/// never a type-erased error.
#[derive(Debug, thiserror::Error)]
pub enum JobsError {
    /// Required arguments are missing: `names` holds them in the call's
    /// order, so one name reads `a string` and several read `strings`.
    #[error("Give {} as {}.", list_names(names), string_word(names))]
    ArgumentsMissing {
        /// The missing argument names, in the call's order.
        names: Vec<String>,
    },
    /// Arguments that must be strings are not: `names` holds them in the
    /// call's order, so one name reads `a string` and several `strings`.
    #[error("{} must be {}.", list_names(names), string_word(names))]
    ArgumentsNotStrings {
        /// The wrongly typed argument names, in the call's order.
        names: Vec<String>,
    },
    /// An extension-owned delegate start names arguments only the model
    /// tool may pass: one `` `/key`: not allowed `` line per key.
    #[error("{}", allowed_lines(keys))]
    ArgumentsNotAllowed {
        /// The disallowed argument keys, in the call's order.
        keys: Vec<String>,
    },
    /// The isolation argument is not one of the supported modes.
    #[error("`isolation` must be `none` or `worktree`.")]
    IsolationUnknown,
    /// The delegate's worktree could not be made.
    #[error("Starting the delegate failed: {}", failure.message)]
    DelegateWorktree {
        /// The creation refusal, with its stable code and safe message.
        failure: Failure,
    },
    /// `timeout_ms` is not a positive integer.
    #[error("`timeout_ms` must be an integer of 1 or more.")]
    TimeoutNotPositive,
    /// The delegate start names no model.
    #[error("Give model: a role or a full reference.\n{catalogue}")]
    ModelMissing {
        /// The valid roles and references the call lists.
        catalogue: String,
    },
    /// The delegate start's model is not a string.
    #[error("model must be a string.\n{catalogue}")]
    ModelNotString {
        /// The valid roles and references the call lists.
        catalogue: String,
    },
    /// The delegate start's model resolves to nothing.
    #[error("{reason}\n{catalogue}")]
    ModelUnresolved {
        /// Why the reference did not resolve.
        reason: String,
        /// The valid roles and references the call lists.
        catalogue: String,
    },
    /// The delegate start's model names labels with no usable credential.
    /// The case carries the site's message text with the fixed code
    /// `credential_missing`.
    #[error("{message}")]
    CredentialUnavailable {
        /// What the credential check reported.
        message: String,
    },
    /// The fork's point could not be read: nothing started. The code is
    /// the point reader's, so a point that cannot fork fails its own code.
    #[error("Starting the fork failed: {}", failure.message)]
    ForkPoint {
        /// The point reader's own refusal, with its code and message.
        failure: Failure,
    },
    /// The session is already at the delegate depth cap.
    #[error(
        "This delegate is at depth {}, the deepest a delegate can be, so it cannot use `{tool}`; do the work in this session.",
        crate::delegate::DEPTH_CAP
    )]
    DepthExceeded {
        /// The refused tool's name.
        tool: String,
    },
    /// An extension-owned delegate start found no listening session.
    #[error("No session is listening for this delegate's end, so nothing was started.")]
    NoInbox,
    /// The delegate child could not be spawned.
    #[error("Starting the delegate failed: {source}.")]
    SpawnFailed {
        /// Why the spawn failed.
        #[source]
        source: io::Error,
    },
    /// The session already runs the maximum delegates.
    #[error(
        "This session already runs {} delegates, the most it can run at once, so nothing was started; wait for one with `jobs wait` or stop one with `jobs stop`, then try again.",
        crate::registry::runs::DELEGATE_CAP
    )]
    DelegatesFull,
    /// An extension-owned delegate start arrived after an end path began
    /// its sweep: a normal end, a shutdown or a rewind. Nothing was
    /// started.
    #[error("The session is ending, so no delegate was started.")]
    ExtensionStartsClosed,
    /// The steer or resume message is empty or all whitespace.
    #[error("`message` must not be empty.")]
    MessageEmpty,
    /// No job has this id.
    #[error("No job has id `{}`.", id.0)]
    UnknownJob {
        /// The unknown job's id.
        id: JobId,
    },
    /// The job is not a delegate.
    #[error("Job `{}` is not a delegate.", id.0)]
    NotDelegate {
        /// The job's id.
        id: JobId,
    },
    /// The delegate is finishing its run and did not take the message.
    #[error(
        "Delegate {} is finishing its run and did not take the message; send it again once it has finished.",
        id.0
    )]
    SteerClosing {
        /// The delegate's job id.
        id: JobId,
    },
    /// The delegate refused the message with a code a tool call may
    /// carry, kept as data. The code is the caller's: the delegate made
    /// it (`message_refused` or `hook_failed`), and this case forwards it.
    #[error("Delegate {} refused the message: {}", id.0, failure.message)]
    SteerRefused {
        /// The delegate's job id.
        id: JobId,
        /// The delegate's own refusal, with its code and message.
        failure: Failure,
    },
    /// The delegate refused the message with any other code. Jobs picks
    /// the fixed code `tool_error` and names the delegate's code in the
    /// text, so this is not a forwarding case.
    #[error(
        "Delegate {} refused the message ({}): {}",
        id.0,
        delegate_code,
        message
    )]
    SteerRefusedOther {
        /// The delegate's job id.
        id: JobId,
        /// The delegate's code, named for the message.
        delegate_code: String,
        /// The delegate's message.
        message: String,
    },
    /// A copy reached the delegate's run, but its answer was lost.
    #[error(
        "Delegate {} received the message, but its answer was lost; it joins the delegate's turn unless the delegate refused it.",
        id.0
    )]
    AnswerLost {
        /// The delegate's job id.
        id: JobId,
    },
    /// A copy may have joined a run that has since ended.
    #[error(
        "Delegate {} ended before answering; the message may have joined its last turn. Send it again to resume the delegate.",
        id.0
    )]
    Unanswered {
        /// The delegate's job id.
        id: JobId,
    },
    /// The delegate's run count is exhausted: no start ran.
    #[error(
        "Delegate {} cannot run again: its run count is exhausted.",
        id.0
    )]
    ResumeExhausted {
        /// The delegate's job id.
        id: JobId,
    },
    /// The delegate log's last line could not be read: nothing started.
    #[error(
        "Resuming delegate {} failed: its log could not be read: {reason}. Nothing was started.",
        id.0
    )]
    ResumeBoundary {
        /// The delegate's job id.
        id: JobId,
        /// Why the log's boundary could not be read; it names the path.
        reason: String,
    },
    /// The resumed run's child could not be spawned: nothing started.
    #[error("Resuming the delegate failed: {source}.")]
    ResumeSpawn {
        /// Why the spawn failed.
        #[source]
        source: io::Error,
    },
    /// `action` names no `jobs` command.
    #[error("`action` must be `list`, `wait`, `write` or `stop`.")]
    ActionUnknown,
    /// `job_id` is present but not a string.
    #[error("`job_id` must be a string.")]
    JobIdNotString,
    /// The call needs `job_id` and has none.
    #[error("Give the job as `job_id`.")]
    JobIdMissing,
    /// The call needs `timeout_ms` and has none.
    #[error("Give how long to wait as `timeout_ms`.")]
    TimeoutMissing,
    /// `timeout_ms` is not an integer of 0 or more.
    #[error("`timeout_ms` must be an integer of 0 or more.")]
    TimeoutNotInteger,
    /// `input` is present but not a string.
    #[error("`input` must be a string.")]
    InputNotString,
    /// `write` has no `input`.
    #[error("Give what to type as `input`.")]
    InputMissing,
    /// `write` asks to wait longer than the ceiling.
    #[error("`timeout_ms` for `write` is at most {max}.")]
    WriteWaitTooLong {
        /// The ceiling, in milliseconds.
        max: u64,
    },
    /// The job was not started with `tty`, so nothing can be typed into
    /// it. The id carries no backticks, as the site prints it.
    #[error("Job {} was not started with `tty`.", id.0)]
    NotTty {
        /// The job's id.
        id: JobId,
    },
    /// The job has already ended.
    #[error("Job `{}` has ended: {status}.", id.0)]
    JobEnded {
        /// The job's id.
        id: JobId,
        /// The word for how it ended: `completed`, `failed` or
        /// `cancelled`.
        status: &'static str,
    },
    /// Typing into the job's terminal failed.
    #[error("Writing to job {} failed: {source}.", id.0)]
    WriteFailed {
        /// The job's id.
        id: JobId,
        /// Why the write failed.
        #[source]
        source: io::Error,
    },
    /// The delegate passed its deadline and was stopped.
    #[error("The delegate passed its `timeout_ms` and was stopped.")]
    DelegateTimedOut,
    /// The delegate's log passed the output cap and was stopped.
    #[error("The delegate's log passed the output cap and the job was stopped.")]
    DelegateOutputCap,
    /// A signal ended the delegate.
    #[error("Killed by {signal}.")]
    DelegateKilled {
        /// The signal's name.
        signal: String,
    },
    /// The delegate ended without a result: no `fiber_exited` was seen.
    #[error("The delegate ended without a result.")]
    DelegateNoResult,
    /// The delegate exited nonzero with no other cause blamed.
    #[error("Exit code {code}.")]
    DelegateExitCode {
        /// The exit code, with absence read as 0.
        code: i32,
    },
    /// The job ended without a result: a runner that returned without
    /// reporting would otherwise leave the job running.
    #[error("The job ended without a result.")]
    JobNoResult,
}

impl JobsError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm, so a new case does not compile until it has one.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::ArgumentsMissing { .. } => ErrorCode::InvalidArguments,
            Self::ArgumentsNotStrings { .. } => ErrorCode::InvalidArguments,
            Self::IsolationUnknown => ErrorCode::InvalidArguments,
            Self::DelegateWorktree { failure } => failure.code.clone(),
            Self::ArgumentsNotAllowed { .. } => ErrorCode::InvalidArguments,
            Self::TimeoutNotPositive => ErrorCode::InvalidArguments,
            Self::ModelMissing { .. } => ErrorCode::InvalidArguments,
            Self::ModelNotString { .. } => ErrorCode::InvalidArguments,
            Self::ModelUnresolved { .. } => ErrorCode::InvalidArguments,
            Self::CredentialUnavailable { .. } => ErrorCode::CredentialMissing,
            Self::ForkPoint { failure } => failure.code.clone(),
            Self::DepthExceeded { .. } => ErrorCode::DepthExceeded,
            Self::NoInbox => ErrorCode::ToolError,
            Self::SpawnFailed { .. } => ErrorCode::ToolError,
            Self::DelegatesFull => ErrorCode::ToolError,
            Self::ExtensionStartsClosed => ErrorCode::Closing,
            Self::MessageEmpty => ErrorCode::InvalidArguments,
            Self::UnknownJob { .. } => ErrorCode::InvalidArguments,
            Self::NotDelegate { .. } => ErrorCode::InvalidArguments,
            Self::SteerClosing { .. } => ErrorCode::Closing,
            Self::SteerRefused { failure, .. } => failure.code.clone(),
            Self::SteerRefusedOther { .. } => ErrorCode::ToolError,
            Self::AnswerLost { .. } => ErrorCode::Indeterminate,
            Self::Unanswered { .. } => ErrorCode::Indeterminate,
            Self::ResumeExhausted { .. } => ErrorCode::ToolError,
            Self::ResumeBoundary { .. } => ErrorCode::ToolError,
            Self::ResumeSpawn { .. } => ErrorCode::ToolError,
            Self::ActionUnknown => ErrorCode::InvalidArguments,
            Self::JobIdNotString => ErrorCode::InvalidArguments,
            Self::JobIdMissing => ErrorCode::InvalidArguments,
            Self::TimeoutMissing => ErrorCode::InvalidArguments,
            Self::TimeoutNotInteger => ErrorCode::InvalidArguments,
            Self::InputNotString => ErrorCode::InvalidArguments,
            Self::InputMissing => ErrorCode::InvalidArguments,
            Self::WriteWaitTooLong { .. } => ErrorCode::InvalidArguments,
            Self::NotTty { .. } => ErrorCode::InvalidArguments,
            Self::JobEnded { .. } => ErrorCode::InvalidArguments,
            Self::WriteFailed { .. } => ErrorCode::ToolError,
            Self::DelegateTimedOut => ErrorCode::Timeout,
            Self::DelegateOutputCap => ErrorCode::OutputCap,
            Self::DelegateKilled { .. } => ErrorCode::Signal,
            Self::DelegateNoResult => ErrorCode::Indeterminate,
            Self::DelegateExitCode { .. } => ErrorCode::NonzeroExit,
            Self::JobNoResult => ErrorCode::Indeterminate,
        }
    }

    /// The failed `Output` for this error: the message, with a trailing
    /// newline exactly when the caller puts the content on its own line.
    /// The one place a jobs tool `Output`'s failure is built.
    pub(crate) fn output(&self, ends_line: bool) -> Output {
        let mut message = self.to_string();
        if ends_line {
            message.push('\n');
        }
        Output {
            content: vec![ContentPart::Text { text: message }],
            error: Some(Failure::from(self)),
            ..Output::default()
        }
    }
}

impl From<&JobsError> for Failure {
    fn from(error: &JobsError) -> Self {
        Failure {
            code: error.code(),
            message: error.to_string(),
            retry_after_ms: None,
            provider: None,
        }
    }
}

/// Argument names as the call sites list them: `a, b`.
fn list_names(names: &[String]) -> String {
    names.join(", ")
}

/// `a string` for one name, `strings` for several.
fn string_word(names: &[String]) -> &'static str {
    if names.len() == 1 {
        "a string"
    } else {
        "strings"
    }
}

/// One `` `/key`: not allowed `` line per key, in the call's order.
fn allowed_lines(keys: &[String]) -> String {
    keys.iter()
        .map(|key| format!("`/{key}`: not allowed"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
