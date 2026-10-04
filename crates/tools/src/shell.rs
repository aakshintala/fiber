//! The shell tool (`docs/tools.md`, "Shell"): run a command, bound its output,
//! and stop its process group on timeout or cancel.

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Failure, Process};
use contract::tool::{Bound, Cancel, Effects, Output, Tool};
use rustix::process::Signal;
use serde_json::{Map, Value, json};

#[path = "shell/command.rs"]
mod command;

#[path = "shell/read_only.rs"]
mod read_only;

#[path = "shell/classify.rs"]
mod classify;

use classify::classify;
use command::{Finished, StopKind};

/// The default when the model gives no `timeout_ms`: 10 minutes.
const DEFAULT_TIMEOUT_MS: u64 = 600_000;

const BARE_WAIT: &str = "This command waits with `sleep` for 25 seconds or more. \
     Use `run_in_background`, wait with `jobs wait`, or run a monitor with an `until` loop.";

const CANCELLED_BEFORE: &str = "Cancelled before it started.";
const CANCELLED: &str = "Cancelled and stopped.";
const INDETERMINATE: &str = "The command was stopped, and Fiber cannot tell whether it completed.";
const HELD_OPEN: &str = "Output was still held open.";

/// One shell tool. Each call has its own process; concurrent calls share nothing mutable.
pub struct Shell {
    workspace: PathBuf,
    clock: Arc<dyn Clock>,
}

impl Shell {
    /// Runs commands in `workspace`, with deadlines read from `clock`.
    pub fn new(workspace: PathBuf, clock: Arc<dyn Clock>) -> Self {
        Self { workspace, clock }
    }
}

impl Tool for Shell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".to_owned(),
            description: "Runs a command in a new process session. `command` is the command. \
                 `workdir` defaults to the workspace; a relative path is resolved against it. \
                 `timeout_ms` defaults to 600000. Standard output and standard error are one stream."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command to run."
                    },
                    "workdir": {
                        "type": "string",
                        "description": "Where to run it. A relative path is resolved against the workspace."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "How long the command may run, in milliseconds. The default is 600000."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            deferred: false,
        }
    }

    fn effects(
        &self,
        arguments: &Map<String, Value>,
    ) -> Result<Effects, contract::tool::EffectsError> {
        // A call `parse` rejects never runs. The closed default keeps a bad
        // `timeout_ms` or `workdir` off the read-only fast path.
        match parse(arguments, &self.workspace) {
            Ok(parsed) => Ok(classify(&parsed.command, &parsed.workdir)),
            Err(_) => Ok(classify::executes(None, None)),
        }
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, emit: &dyn Emit) -> Output {
        if cancel.is_cancelled() {
            return line_only(CANCELLED_BEFORE);
        }
        let parsed = match parse(arguments, &self.workspace) {
            Ok(parsed) => parsed,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        if bare_wait(&parsed.command) {
            return failed(ErrorCode::InvalidArguments, BARE_WAIT.to_owned());
        }
        let program = shell_program();
        from_spawn(
            command::execute(
                program,
                &parsed.command,
                &parsed.workdir,
                Duration::from_millis(parsed.timeout_ms),
                self.clock.as_ref(),
                cancel,
                emit,
            ),
            parsed.timeout_ms,
        )
    }

    fn bound(&self) -> Bound {
        Bound {
            start: 8192,
            end: 8192,
        }
    }
}

struct Parsed {
    command: String,
    workdir: PathBuf,
    timeout_ms: u64,
}

fn parse(arguments: &Map<String, Value>, workspace: &Path) -> Result<Parsed, String> {
    let command = match arguments.get("command") {
        Some(Value::String(command)) => command.clone(),
        Some(_) => return Err("`command` must be a string.".to_owned()),
        None => return Err("Give the command to run as `command`.".to_owned()),
    };
    let workdir = workdir(arguments, workspace)?;
    let timeout_ms = match arguments.get("timeout_ms") {
        None => DEFAULT_TIMEOUT_MS,
        Some(value) => timeout_ms(value)?,
    };
    Ok(Parsed {
        command,
        workdir,
        timeout_ms,
    })
}

fn workdir(arguments: &Map<String, Value>, workspace: &Path) -> Result<PathBuf, String> {
    let path = match arguments.get("workdir") {
        None => workspace.to_path_buf(),
        Some(Value::String(raw)) => {
            let given = Path::new(raw);
            if given.is_absolute() {
                given.to_path_buf()
            } else {
                workspace.join(given)
            }
        }
        Some(_) => return Err("`workdir` must be a string.".to_owned()),
    };
    if path.is_dir() {
        Ok(path)
    } else {
        Err(format!(
            "`{}` is not a directory. Give a directory as `workdir`.",
            path.display()
        ))
    }
}

fn timeout_ms(value: &Value) -> Result<u64, String> {
    let Some(number) = value.as_number() else {
        return Err("`timeout_ms` must be an integer number of milliseconds.".to_owned());
    };
    // Signed first, so 0 is accepted and a negative is rejected. Checking
    // `as_u64` first would make `<` and `<=` agree on every value.
    let Some(ms) = number.as_i64() else {
        return Err("`timeout_ms` must be an integer number of milliseconds.".to_owned());
    };
    if ms < 0 {
        return Err("`timeout_ms` is negative. Give 0 or more milliseconds.".to_owned());
    }
    u64::try_from(ms)
        .map_err(|_| "`timeout_ms` must be an integer number of milliseconds.".to_owned())
}

/// The first part is the text before the first `;`, `&`, `|` or newline.
/// It is a bare wait when that part is `sleep` and one duration of 25 seconds
/// or more.
pub(crate) fn bare_wait(command: &str) -> bool {
    let mut words = first_part(command).split_whitespace();
    let Some(word) = words.next() else {
        return false;
    };
    let Some(duration) = words.next() else {
        return false;
    };
    if words.next().is_some() || word != "sleep" {
        return false;
    }
    duration_seconds(duration).is_some_and(|seconds| seconds >= 25.0)
}

fn first_part(command: &str) -> &str {
    let end = command.find(['\n', ';', '&', '|']).unwrap_or(command.len());
    command.get(..end).unwrap_or(command).trim()
}

fn duration_seconds(token: &str) -> Option<f64> {
    let (number, factor) = match token.chars().next_back() {
        Some('s') => (strip_last(token)?, 1.0),
        Some('m') => (strip_last(token)?, 60.0),
        Some('h') => (strip_last(token)?, 3_600.0),
        Some('d') => (strip_last(token)?, 86_400.0),
        Some(last) => {
            // A digit or a trailing dot is seconds. `inf` parses as a
            // number, so a tail that accepts it would treat `sleep inf`
            // as a bare wait.
            if !bare_tail(last) {
                return None;
            }
            (token, 1.0)
        }
        None => return None,
    };
    // A negative or non-finite parse fails `seconds >= 25` in `bare_wait`,
    // so it is not rejected again here.
    number.parse::<f64>().ok().map(|value| value * factor)
}

fn bare_tail(last: char) -> bool {
    last.is_ascii_digit() || last == '.'
}

fn strip_last(token: &str) -> Option<&str> {
    token.get(..token.len().saturating_sub(1))
}

fn shell_program() -> &'static Path {
    if Path::new("/bin/bash").is_file() {
        Path::new("/bin/bash")
    } else {
        Path::new("sh")
    }
}

fn from_spawn(result: Result<Finished, std::io::Error>, timeout_ms: u64) -> Output {
    match result {
        Ok(finished) => assemble(timeout_ms, finished),
        Err(err) => failed(
            ErrorCode::ToolError,
            format!("The command could not be started: {err}."),
        ),
    }
}

fn assemble(timeout_ms: u64, finished: Finished) -> Output {
    if finished.indeterminate {
        let timed_out = finished.stop == Some(StopKind::Timeout);
        return with_process(
            text_of(&finished.output, INDETERMINATE),
            Some(failure(ErrorCode::Indeterminate, INDETERMINATE.to_owned())),
            process_of(&finished, timed_out),
        );
    }
    if finished.stop == Some(StopKind::Timeout) {
        let line = timeout_line(timeout_ms);
        return with_process(
            text_of(&finished.output, &line),
            Some(failure(ErrorCode::Timeout, line)),
            process_of(&finished, true),
        );
    }
    if finished.stop == Some(StopKind::Cancel) {
        return with_process(
            text_of(&finished.output, CANCELLED),
            None,
            process_of(&finished, false),
        );
    }
    let Some(status) = finished.status else {
        return failed(
            ErrorCode::ToolError,
            "The command's process could not be reaped.".to_owned(),
        );
    };
    let (exit_code, signal, line) = observed(status);
    let mut text = text_of(&finished.output, &line);
    if finished.held_open {
        text.push_str(HELD_OPEN);
        text.push('\n');
    }
    let error = if signal.is_some() && !finished.sent_signal {
        Some(failure(ErrorCode::Signal, line))
    } else if exit_code.is_some_and(|code| code != 0) {
        Some(failure(ErrorCode::NonzeroExit, line))
    } else {
        None
    };
    with_process(
        text,
        error,
        Process {
            exit_code,
            signal,
            timed_out: false,
        },
    )
}

fn observed(status: ExitStatus) -> (Option<i32>, Option<String>, String) {
    if let Some(number) = status.signal() {
        let name = signal_name(number);
        let line = format!("Killed by {name}.");
        return (None, Some(name), line);
    }
    let code = status.code().unwrap_or(0);
    (Some(code), None, exit_line(code))
}

fn process_of(finished: &Finished, timed_out: bool) -> Process {
    match finished.status {
        Some(status) => {
            let (exit_code, signal, _) = observed(status);
            Process {
                exit_code,
                signal,
                timed_out,
            }
        }
        None => Process {
            exit_code: None,
            signal: None,
            timed_out,
        },
    }
}

pub(crate) fn exit_line(code: i32) -> String {
    format!("Exit code {code}.")
}

pub(crate) fn timeout_line(timeout_ms: u64) -> String {
    format!("Timed out after {timeout_ms} ms and stopped.")
}

/// The signal's name, such as `SIGKILL`.
pub(crate) fn signal_name(number: i32) -> String {
    for (signal, name) in KNOWN {
        if signal.as_raw() == number {
            return (*name).to_owned();
        }
    }
    format!("SIG{number}")
}

const KNOWN: &[(Signal, &str)] = &[
    (Signal::HUP, "SIGHUP"),
    (Signal::INT, "SIGINT"),
    (Signal::QUIT, "SIGQUIT"),
    (Signal::ILL, "SIGILL"),
    (Signal::TRAP, "SIGTRAP"),
    (Signal::ABORT, "SIGABRT"),
    (Signal::BUS, "SIGBUS"),
    (Signal::FPE, "SIGFPE"),
    (Signal::KILL, "SIGKILL"),
    (Signal::USR1, "SIGUSR1"),
    (Signal::SEGV, "SIGSEGV"),
    (Signal::USR2, "SIGUSR2"),
    (Signal::PIPE, "SIGPIPE"),
    (Signal::ALARM, "SIGALRM"),
    (Signal::TERM, "SIGTERM"),
    (Signal::CHILD, "SIGCHLD"),
    (Signal::CONT, "SIGCONT"),
    (Signal::STOP, "SIGSTOP"),
    (Signal::TSTP, "SIGTSTP"),
    (Signal::TTIN, "SIGTTIN"),
    (Signal::TTOU, "SIGTTOU"),
    (Signal::URG, "SIGURG"),
    (Signal::XCPU, "SIGXCPU"),
    (Signal::XFSZ, "SIGXFSZ"),
    (Signal::VTALARM, "SIGVTALRM"),
    (Signal::PROF, "SIGPROF"),
    (Signal::WINCH, "SIGWINCH"),
    (Signal::SYS, "SIGSYS"),
];

fn text_of(output: &[u8], line: &str) -> String {
    let mut text = String::from_utf8_lossy(output).into_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    text
}

fn line_only(line: &str) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: format!("{line}\n"),
        }],
        ..Output::default()
    }
}

fn with_process(text: String, error: Option<Failure>, process: Process) -> Output {
    Output {
        content: vec![ContentPart::Text { text }],
        error,
        process: Some(process),
        ..Output::default()
    }
}

fn failed(code: ErrorCode, message: String) -> Output {
    Output {
        content: vec![ContentPart::Text {
            text: format!("{message}\n"),
        }],
        error: Some(failure(code, message)),
        ..Output::default()
    }
}

fn failure(code: ErrorCode, message: String) -> Failure {
    Failure {
        code,
        message,
        retry_after: None,
        provider: None,
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
