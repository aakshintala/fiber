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
    /// `timeout_ms` is given with `monitor`, which takes `deadline_ms`.
    #[error("A monitor's limit is `deadline_ms`.")]
    MonitorTimeout,
    /// `deadline_ms` is given without `monitor`.
    #[error("`deadline_ms` is a monitor's deadline; set `monitor: true` with it.")]
    DeadlineWithoutMonitor,
    /// `deadline_ms` is past the longest a monitor may run in this session.
    #[error(
        "`deadline_ms` is more than {max_ms}, the longest a monitor may run here. Give {max_ms} or less."
    )]
    DeadlineTooLong {
        /// The longest deadline in this session, in milliseconds.
        max_ms: u64,
    },
    /// A background mode was asked for in a session with no jobs.
    #[error("Background jobs are not available in this session.")]
    NoJobs,
    /// The command is a bare wait a background mode should run.
    #[error(
        "This command waits with `sleep` for 25 seconds or more. \
         Use `run_in_background`, wait with `jobs wait`, or run a monitor with an `until` loop."
    )]
    BareWait,
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

impl ShellError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::Arguments(error) => error.code(),
            Self::NotADirectory { .. }
            | Self::MonitorAlone
            | Self::MonitorTimeout
            | Self::DeadlineWithoutMonitor
            | Self::DeadlineTooLong { .. }
            | Self::NoJobs
            | Self::BareWait => ErrorCode::InvalidArguments,
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
