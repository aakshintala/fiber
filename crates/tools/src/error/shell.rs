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
    #[error("{}", crate::shell::timeout_line(*ms, *has_pipe))]
    TimedOut {
        /// The timeout in milliseconds.
        ms: u64,
        /// The command text holds a `|`: a pipeline stage may hold its
        /// output until it exits, which the stop then loses.
        has_pipe: bool,
    },
    /// A moved job ran past the timeout it kept and was stopped: the same
    /// line as [`TimedOut`], plus how to keep a long-running process
    /// instead. Foreground kills keep [`TimedOut`] exactly.
    #[error(
        "{} {}",
        crate::shell::timeout_line(*ms, *has_pipe),
        crate::shell::MOVED_JOB_HINT
    )]
    TimedOutMoved {
        /// The timeout in milliseconds.
        ms: u64,
        /// The command text holds a `|`: a pipeline stage may hold its
        /// output until it exits, which the stop then loses.
        has_pipe: bool,
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
    let check = usable_check(command).unwrap_or_else(|| CHECK_PLACEHOLDER.to_owned());
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
/// rest past the refused command's first separator, with `#` comments
/// removed and only lines that are blank outside quotes dropped, when its
/// first code-bearing line opens with neither a separator nor a loop
/// keyword and no later line opens with a separator. A rest holding a
/// newline inside quotes names the placeholder instead: only an unquoted
/// multiline check is named, never a quoted one. Every kept line stays
/// byte for byte — leading spaces and blank lines within a multiline quote
/// included — so the emitted `until` snippet polls the check as written
/// and stays valid shell no `#` comments out.
fn usable_check(command: &str) -> Option<String> {
    let end = command.find(['\n', ';', '&', '|'])?;
    let rest = command.get(end + 1..)?.trim();
    let cleaned = strip_shell_comments(rest)?;
    // Fallback: a newline inside quotes cannot be named verbatim, so the
    // refusal names the placeholder instead. Quote state carries across
    // newlines exactly as the comment strip tracks it.
    {
        let mut in_single = false;
        let mut in_double = false;
        let mut escaped = false;
        for ch in cleaned.chars() {
            if ch == '\n' && (in_single || in_double) {
                return None;
            }
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' if !in_single => escaped = true,
                '\'' if !in_double => in_single = !in_single,
                '"' if !in_single => in_double = !in_double,
                _ => {}
            }
        }
    }
    // The fallback above rejected any newline inside quotes, so every
    // blank line here starts outside quotes: drop blank lines and keep
    // every other line byte for byte. Trimmed views serve the
    // accept/reject tests only.
    let kept: Vec<&str> = cleaned
        .split('\n')
        .filter(|line| line.chars().any(|ch| !ch.is_whitespace()))
        .collect();
    let first = kept.first()?;
    let trimmed = first.trim();
    if matches!(trimmed.chars().next(), Some('&' | '|' | ';')) {
        return None;
    }
    if matches!(
        trimmed.split_whitespace().next(),
        Some("while" | "until" | "for")
    ) {
        return None;
    }
    if kept.get(1..).is_some_and(|later| {
        later
            .iter()
            .any(|line| matches!(line.trim().chars().next(), Some('&' | '|' | ';')))
    }) {
        return None;
    }
    // Stripping an end-of-rest comment leaves its preceding space behind;
    // trim the end back off. The tail is always outside quotes — a balanced
    // check cannot end inside them — so no quoted byte is touched.
    Some(kept.join("\n").trim_end().to_owned())
}

/// The rest before its `#` comments: a `#` outside quotes starts a comment
/// only at the rest start or after whitespace or a shell separator, so a `#`
/// inside quotes or a word stays part of the check command. Quote state
/// carries across newlines, so a `#` on a later line inside a multiline
/// quote is kept. `None` when a quote or escape is left open: emitting that
/// rest would split a quoted string, so the refusal falls back to
/// `<check command>` instead of handing the model invalid shell.
fn strip_shell_comments(rest: &str) -> Option<String> {
    let mut cleaned = String::with_capacity(rest.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut in_comment = false;
    let mut previous: Option<char> = None;
    for ch in rest.chars() {
        if in_comment {
            if ch == '\n' {
                in_comment = false;
                cleaned.push(ch);
            }
            previous = Some(ch);
            continue;
        }
        if escaped {
            escaped = false;
            cleaned.push(ch);
            previous = Some(ch);
            continue;
        }
        match ch {
            '\\' if !in_single => {
                escaped = true;
                cleaned.push(ch);
                previous = Some(ch);
            }
            '\'' if !in_double => {
                in_single = !in_single;
                cleaned.push(ch);
                previous = Some(ch);
            }
            '"' if !in_single => {
                in_double = !in_double;
                cleaned.push(ch);
                previous = Some(ch);
            }
            '#' if !in_single && !in_double => {
                let at_comment_start = match previous {
                    None => true,
                    Some(prev) => {
                        prev.is_whitespace() || matches!(prev, ';' | '&' | '|' | '(' | '`')
                    }
                };
                if at_comment_start {
                    in_comment = true;
                } else {
                    cleaned.push(ch);
                }
                previous = Some(ch);
            }
            _ => {
                cleaned.push(ch);
                previous = Some(ch);
            }
        }
    }
    if in_single || in_double || escaped {
        return None;
    }
    Some(cleaned)
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
            Self::TimedOut { .. } | Self::TimedOutMoved { .. } | Self::DeadlinePassed { .. } => {
                ErrorCode::Timeout
            }
            Self::Killed { .. } => ErrorCode::Signal,
            Self::Exited { .. } => ErrorCode::NonzeroExit,
            Self::Capped => ErrorCode::OutputCap,
            Self::Flooded => ErrorCode::Flooded,
        }
    }
}
