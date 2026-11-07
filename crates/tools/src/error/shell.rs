//! Failures of the shell tool (`docs/tools.md`, "Shell"): the arguments, the
//! start of a command, and how a command or a job ended.

use std::io;
use std::path::PathBuf;

use contract::ErrorCode;

use super::ArgumentsError;

/// Every way the shell tool fails. Each case keeps the code `docs/errors.md`
/// gives its message, and a message another line repeats reads the same
/// function or const that line does.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ShellError {
    /// The call's arguments do not fit the schema.
    #[error(transparent)]
    Arguments(#[from] ArgumentsError),
    /// `workdir` names a path that is not a directory.
    #[error("`{}` is not a directory. Give a directory as `workdir`.", path.display())]
    NotADirectory {
        /// The path `workdir` resolved to.
        path: PathBuf,
    },
    /// `monitor` is combined with `run_in_background` or `tty`.
    #[error("`monitor` cannot be combined with `run_in_background` or `tty`.")]
    MonitorAlone,
    /// `timeout_ms` is past the longest a monitor may run in this session.
    #[error(
        "`timeout_ms` is more than {max_ms}, the longest a monitor may run here. Give {max_ms} or less."
    )]
    DeadlineTooLong {
        /// The longest deadline in this session, in milliseconds.
        max_ms: u64,
    },
    /// `timeout_ms` is below the least a command may run.
    #[error(
        "`timeout_ms` must be at least 1. Leave `timeout_ms` out of a background call that should run until stopped."
    )]
    TimeoutTooSmall,
    /// `detach` is combined with `run_in_background`, `tty` or `monitor`.
    #[error("`detach` cannot be combined with `run_in_background`, `tty` or `monitor`.")]
    DetachAlone,
    /// `detach` is combined with a `timeout_ms`.
    #[error("`detach` takes no `timeout_ms`: Fiber never stops a detached command.")]
    DetachTimeout,
    /// `detach` is combined with `unconfined: false`.
    #[error("`detach` runs unconfined: leave `unconfined` out or give true.")]
    DetachConfined,
    /// A detached call was made in a session with no detached log directory.
    #[error("Detached commands are not available in this session.")]
    NoDetach,
    /// A background mode was asked for in a session with no jobs.
    #[error("Background jobs are not available in this session.")]
    NoJobs,
    /// The command is a bare wait a background mode should run.
    #[error("{}", bare_wait_message(command))]
    BareWait {
        /// The refused command, quoted verbatim in the refusal.
        command: String,
    },
    /// The command could not be spawned.
    #[error("The command could not be started: {source}.")]
    NotStarted {
        /// The spawn failure.
        #[source]
        source: io::Error,
    },
    /// The sandbox could not be applied at spawn, so the call never ran.
    /// A reload that turned the sandbox off after the call was declared
    /// confined, or a policy the kernel refused, ends here: never
    /// unconfined, and never retried.
    #[error("The sandbox could not be applied: {cause}.")]
    SandboxApply {
        /// Why the policy did not apply.
        cause: String,
    },
    /// The command's process could not be reaped after its output closed.
    #[error("The command's process could not be reaped.")]
    NotReaped,
    /// The command was stopped, and whether it completed is not known.
    #[error("{}", crate::shell::INDETERMINATE)]
    Indeterminate,
    /// A command ran past its timeout and was stopped.
    #[error("{}", crate::shell::timeout_line(*ms))]
    TimedOut {
        /// The timeout in milliseconds.
        ms: u64,
    },
    /// A monitor ran past its deadline and was stopped.
    #[error("{}", crate::shell::deadline_line(*ms))]
    DeadlinePassed {
        /// The deadline in milliseconds.
        ms: u64,
    },
    /// The command's process was killed by a signal Fiber did not send.
    #[error("{}", crate::shell::killed_line(signal))]
    Killed {
        /// The signal's name, such as `SIGKILL`.
        signal: String,
    },
    /// The command exited with a nonzero status.
    #[error("{}", crate::shell::exit_line(*code))]
    Exited {
        /// The exit status.
        code: i32,
    },
    /// The output passed the cap and the job was stopped.
    #[error("The output passed 5 GB and the job was stopped.")]
    Capped,
    /// A monitor's output was suppressed too long and the monitor was stopped.
    #[error(
        "The monitor's output was suppressed for 30 seconds, so it was stopped. \
         Restart it with a more selective source."
    )]
    Flooded,
}

/// The placeholder a bare-wait refusal names when the refused command holds
/// no usable check command after its sleep.
const CHECK_PLACEHOLDER: &str = "<check command>";

/// What a bare-wait refusal says: the refused command quoted verbatim, a
/// background rewrite of that same command, and a monitor `until` snippet
/// naming the check command when the command holds one, with the deadline
/// rule the monitor runs under (`docs/tools.md`, "Background jobs").
fn bare_wait_message(command: &str) -> String {
    let check = usable_check(command).unwrap_or(CHECK_PLACEHOLDER);
    format!(
        "`{command}` waits with `sleep` for 25 seconds or more, so it never started. \
         Run it in the background instead: call `shell` again with `run_in_background: true` and the command `{command}`, then wait for it with `jobs wait`. \
         To wait for a condition instead, start a monitor with `monitor: true` and a command like `until {check}; do sleep 5; done`; \
         every monitor has a deadline, 5 minutes ({} ms) by default, at most 30 minutes ({} ms), and at most 10 minutes ({} ms) in a non-interactive run.",
        crate::shell::DEFAULT_DEADLINE_MS,
        crate::shell::MAX_DEADLINE_MS,
        crate::shell::MAX_DEADLINE_NON_INTERACTIVE_MS,
    )
}

/// The check command a bare-wait refusal names in its monitor snippet: the
/// refused command past its first separator, when that rest is more than
/// comments or shell blanks and opens with neither a separator nor a loop
/// keyword, so it reads as the condition to poll for.
fn usable_check(command: &str) -> Option<&str> {
    let end = command.find(['\n', ';', '&', '|'])?;
    let rest = command.get(end + 1..)?.trim();
    if rest.is_empty()
        || rest.starts_with('#')
        || matches!(rest.chars().next(), Some('&' | '|' | ';'))
    {
        return None;
    }
    if matches!(
        rest.split_whitespace().next(),
        Some("while" | "until" | "for")
    ) {
        return None;
    }
    Some(rest)
}

impl ShellError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::Arguments(error) => error.code(),
            Self::NotADirectory { .. }
            | Self::MonitorAlone
            | Self::DeadlineTooLong { .. }
            | Self::TimeoutTooSmall
            | Self::DetachAlone
            | Self::DetachTimeout
            | Self::DetachConfined
            | Self::NoJobs
            | Self::NoDetach
            | Self::BareWait { .. } => ErrorCode::InvalidArguments,
            Self::NotStarted { .. } | Self::NotReaped | Self::SandboxApply { .. } => {
                ErrorCode::ToolError
            }
            Self::Indeterminate => ErrorCode::Indeterminate,
            Self::TimedOut { .. } | Self::DeadlinePassed { .. } => ErrorCode::Timeout,
            Self::Killed { .. } => ErrorCode::Signal,
            Self::Exited { .. } => ErrorCode::NonzeroExit,
            Self::Capped => ErrorCode::OutputCap,
            Self::Flooded => ErrorCode::Flooded,
        }
    }
}
