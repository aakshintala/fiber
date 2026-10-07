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
use contract::jobs::Jobs;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Failure, Process};
use contract::tool::{Bound, Cancel, Effects, Output, Tool};
use rustix::process::Signal;
use serde_json::{Map, Value, json};

#[path = "shell/background.rs"]
mod background;

mod command;
mod drive;
mod moved;
mod process_group;
mod spawn;

#[path = "shell/groups.rs"]
mod groups;

#[path = "shell/monitor.rs"]
mod monitor;

#[path = "shell/output.rs"]
mod output;

#[path = "shell/prelude.rs"]
mod prelude;

#[path = "shell/tty.rs"]
mod tty;

#[path = "shell/read_only.rs"]
mod read_only;

#[path = "shell/classify.rs"]
mod classify;

use classify::classify;
use command::{Finished, StopKind};
pub use groups::kill_every_group;

/// The default when the model gives no `timeout_ms`: 10 minutes.
const DEFAULT_TIMEOUT_MS: u64 = 600_000;

const BARE_WAIT: &str = "This command waits with `sleep` for 25 seconds or more. \
     Use `run_in_background`, wait with `jobs wait`, or run a monitor with an `until` loop.";

/// A monitor's deadline when the model gives no `deadline_ms`: 5 minutes
/// (`docs/tools.md`, "Background jobs").
const DEFAULT_DEADLINE_MS: u64 = 300_000;

/// The longest deadline a monitor may have: 30 minutes.
const MAX_DEADLINE_MS: u64 = 1_800_000;

/// The longest deadline in a non-interactive run: 10 minutes.
const MAX_DEADLINE_NON_INTERACTIVE_MS: u64 = 600_000;

const MONITOR_TIMEOUT: &str = "A monitor's limit is `deadline_ms`.";
const DEADLINE_WITHOUT_MONITOR: &str =
    "`deadline_ms` is a monitor's deadline; set `monitor: true` with it.";
const MONITOR_ALONE: &str = "`monitor` cannot be combined with `run_in_background` or `tty`.";

const NO_JOBS: &str = "Background jobs are not available in this session.";

const CANCELLED_BEFORE: &str = "Cancelled before it started.";
const CANCELLED: &str = "Cancelled and stopped.";
const INDETERMINATE: &str = "The command was stopped, and Fiber cannot tell whether it completed.";
const HELD_OPEN: &str = "Output was still held open.";

/// One shell tool. Each call has its own process; concurrent calls share nothing mutable.
pub struct Shell {
    workspace: PathBuf,
    clock: Arc<dyn Clock>,
    /// The Fiber binary whose hidden search subcommands `grep` and `find`
    /// run in commands; none keeps the system tools.
    search: Option<PathBuf>,
    /// When set, a long command, `run_in_background`, or a shell that exits
    /// with members left becomes a job. Without it, nothing moves.
    jobs: Option<Arc<dyn Jobs>>,
    /// The longest `deadline_ms` a monitor may have.
    max_deadline_ms: u64,
}

impl Shell {
    /// Runs commands in `workspace`, with deadlines read from `clock`.
    pub fn new(workspace: PathBuf, clock: Arc<dyn Clock>) -> Self {
        Self {
            workspace,
            clock,
            search: None,
            jobs: None,
            max_deadline_ms: MAX_DEADLINE_MS,
        }
    }

    /// A shell for a non-interactive run: a monitor's deadline is at most
    /// 10 minutes (`docs/tools.md`, "Background jobs").
    pub fn non_interactive(self) -> Self {
        Self {
            max_deadline_ms: MAX_DEADLINE_NON_INTERACTIVE_MS,
            ..self
        }
    }

    /// Routes `grep` and `find` in commands through the Fiber binary's
    /// hidden search subcommands (`docs/tools.md`, "Search").
    pub fn with_search(self, fiber: PathBuf) -> Self {
        Self {
            search: Some(fiber),
            ..self
        }
    }

    /// Moves long commands, `run_in_background`, and a shell that exits with
    /// members left into `jobs` (`docs/tools.md`, "Moving to the background").
    pub fn with_jobs(self, jobs: Arc<dyn Jobs>) -> Self {
        Self {
            jobs: Some(jobs),
            ..self
        }
    }
}

impl Tool for Shell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".to_owned(),
            description: "Runs a command in a new process session. `command` is the command. \
                 `workdir` defaults to the workspace; a relative path is resolved against it. \
                 `timeout_ms` defaults to 600000. Standard output and standard error are one stream, \
                 except in a monitor, whose standard error goes to its own file."
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
                    },
                    "run_in_background": {
                        "type": "boolean",
                        "description": "Start the command as a job and return its receipt at once."
                    },
                    "tty": {
                        "type": "boolean",
                        "description": "Run the command in a pseudo-terminal, as a job, and return its receipt with the first 250 ms of output. Type into it with `jobs` `write`."
                    },
                    "monitor": {
                        "type": "boolean",
                        "description": "Start the command as a monitor: a job whose standard output lines reach you in batches. Standard error goes to its own file."
                    },
                    "deadline_ms": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "A monitor's deadline, in milliseconds. The default is 300000; the most is 1800000, or 600000 in a non-interactive run."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            deferred: false, hosted: None,
        }
    }

    fn effects(
        &self,
        arguments: &Map<String, Value>,
    ) -> Result<Effects, contract::tool::EffectsError> {
        // A call `parse` rejects never runs. The closed default keeps a bad
        // `timeout_ms` or `workdir` off the read-only fast path.
        match parse(arguments, &self.workspace, self.max_deadline_ms) {
            Ok(parsed) => Ok(classify(&parsed.command, &parsed.workdir)),
            Err(_) => Ok(classify::executes(None, None)),
        }
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, emit: &dyn Emit) -> Output {
        if cancel.is_cancelled() {
            return line_only(CANCELLED_BEFORE);
        }
        let parsed = match parse(arguments, &self.workspace, self.max_deadline_ms) {
            Ok(parsed) => parsed,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        if parsed.mode != Mode::Foreground && self.jobs.is_none() {
            return failed(ErrorCode::InvalidArguments, NO_JOBS.to_owned());
        }
        if !parsed.mode.in_background() && bare_wait(&parsed.command) {
            return failed(ErrorCode::InvalidArguments, BARE_WAIT.to_owned());
        }
        // The functions reach only the model's own command line: `classify`
        // and `effects` saw the original string above.
        let command = match &self.search {
            Some(fiber) => format!("{}\n{}", prelude::define(Some(fiber)), parsed.command),
            None => parsed.command.clone(),
        };
        let program = shell_program();
        let policy = match (&self.jobs, parsed.mode) {
            (None, _) => command::MovePolicy::Stay,
            (Some(_), Mode::Foreground) => command::MovePolicy::Foreground,
            (Some(_), Mode::Background) => command::MovePolicy::Background,
            (Some(_), Mode::Terminal { .. }) => command::MovePolicy::Terminal,
            (Some(_), Mode::Monitor) => command::MovePolicy::Monitor,
        };
        let limit = Limit {
            ms: parsed.timeout_ms,
            monitor: parsed.mode == Mode::Monitor,
        };
        match command::execute(
            program,
            &command,
            &parsed.workdir,
            Duration::from_millis(parsed.timeout_ms),
            self.clock.as_ref(),
            cancel,
            emit,
            policy,
            self.jobs.as_deref(),
        ) {
            Ok(command::Ran::Finished(finished)) => from_spawn(Ok(finished), limit),
            Ok(command::Ran::Moved(moved)) => match &self.jobs {
                Some(jobs) => background::take(
                    moved,
                    Arc::clone(jobs),
                    Arc::clone(&self.clock),
                    &parsed.command,
                    limit,
                    matches!(parsed.mode, Mode::Terminal { .. }),
                    cancel,
                    emit,
                ),
                None => {
                    let finished = moved.resume(self.clock.as_ref(), cancel, emit);
                    from_spawn(Ok(finished), limit)
                }
            },
            Err(err) => from_spawn(Err(err), limit),
        }
    }

    fn bound(&self) -> Bound {
        Bound {
            start: 8192,
            end: 8192,
        }
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("shell")
    }
}

struct Parsed {
    command: String,
    workdir: PathBuf,
    /// The command's timeout: `timeout_ms`, or a monitor's deadline.
    timeout_ms: u64,
    mode: Mode,
}

/// How a call runs: the arguments `run_in_background`, `tty` and `monitor`,
/// validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// None of them: moves only after 30 seconds or a shell exit.
    Foreground,
    /// `run_in_background`.
    Background,
    /// `tty`, with `run_in_background` or without.
    Terminal { run_in_background: bool },
    /// `monitor`, alone.
    Monitor,
}

impl Mode {
    /// The call set `run_in_background`, the one mode a bare wait may use
    /// (`docs/tools.md`, "Running a command").
    fn in_background(self) -> bool {
        matches!(
            self,
            Self::Background
                | Self::Terminal {
                    run_in_background: true
                }
        )
    }
}

/// How long a command may run, and whether it is a monitor's deadline,
/// which a timeout names as such (`docs/tools.md`, "Background jobs").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limit {
    /// Milliseconds.
    pub(crate) ms: u64,
    /// The limit is a monitor's `deadline_ms`.
    pub(crate) monitor: bool,
}

impl Limit {
    /// What a stop at this limit says.
    pub(crate) fn line(self) -> String {
        if self.monitor {
            deadline_line(self.ms)
        } else {
            timeout_line(self.ms)
        }
    }
}

fn parse(
    arguments: &Map<String, Value>,
    workspace: &Path,
    max_deadline_ms: u64,
) -> Result<Parsed, String> {
    let command = match arguments.get("command") {
        Some(Value::String(command)) => command.clone(),
        Some(_) => return Err("`command` must be a string.".to_owned()),
        None => return Err("Give the command to run as `command`.".to_owned()),
    };
    let workdir = workdir(arguments, workspace)?;
    let given_timeout = arguments
        .get("timeout_ms")
        .map(|value| millis(value, "timeout_ms"))
        .transpose()?;
    let given_deadline = arguments
        .get("deadline_ms")
        .map(|value| millis(value, "deadline_ms"))
        .transpose()?;
    let run_in_background = flag(arguments, "run_in_background")?;
    let tty = flag(arguments, "tty")?;
    let monitor = flag(arguments, "monitor")?;
    let mode = match (monitor, tty, run_in_background) {
        (true, false, false) => Mode::Monitor,
        (true, _, _) => return Err(MONITOR_ALONE.to_owned()),
        (false, true, run_in_background) => Mode::Terminal { run_in_background },
        (false, false, true) => Mode::Background,
        (false, false, false) => Mode::Foreground,
    };
    let timeout_ms = if mode == Mode::Monitor {
        if given_timeout.is_some() {
            return Err(MONITOR_TIMEOUT.to_owned());
        }
        let deadline = given_deadline.unwrap_or(DEFAULT_DEADLINE_MS);
        if deadline > max_deadline_ms {
            return Err(format!(
                "`deadline_ms` is more than {max_deadline_ms}, the longest a monitor may run here. Give {max_deadline_ms} or less."
            ));
        }
        deadline
    } else {
        if given_deadline.is_some() {
            return Err(DEADLINE_WITHOUT_MONITOR.to_owned());
        }
        given_timeout.unwrap_or(DEFAULT_TIMEOUT_MS)
    };
    Ok(Parsed {
        command,
        workdir,
        timeout_ms,
        mode,
    })
}

fn flag(arguments: &Map<String, Value>, name: &str) -> Result<bool, String> {
    match arguments.get(name) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("`{name}` must be a boolean.")),
    }
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

fn millis(value: &Value, name: &str) -> Result<u64, String> {
    let not_integer = || format!("`{name}` must be an integer number of milliseconds.");
    let Some(number) = value.as_number() else {
        return Err(not_integer());
    };
    // Signed first, so 0 is accepted and a negative is rejected. Checking
    // `as_u64` first would make `<` and `<=` agree on every value.
    let Some(ms) = number.as_i64() else {
        return Err(not_integer());
    };
    if ms < 0 {
        return Err(format!(
            "`{name}` is negative. Give 0 or more milliseconds."
        ));
    }
    u64::try_from(ms).map_err(|_| not_integer())
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

fn from_spawn(result: Result<Finished, std::io::Error>, limit: Limit) -> Output {
    match result {
        Ok(finished) => assemble(limit, finished),
        Err(err) => failed(
            ErrorCode::ToolError,
            format!("The command could not be started: {err}."),
        ),
    }
}

fn assemble(limit: Limit, finished: Finished) -> Output {
    if finished.indeterminate {
        let timed_out = finished.stop == Some(StopKind::Timeout);
        return with_process(
            text_of(&finished.output, INDETERMINATE),
            Some(failure(ErrorCode::Indeterminate, INDETERMINATE.to_owned())),
            process_of(&finished, timed_out),
        );
    }
    if finished.stop == Some(StopKind::Timeout) {
        let line = limit.line();
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

/// What a monitor stopped at its deadline says (`docs/tools.md`,
/// "Background jobs").
pub(crate) fn deadline_line(deadline_ms: u64) -> String {
    format!(
        "The monitor's deadline of {deadline_ms} ms passed; start it again if you still need it."
    )
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
        retry_after_ms: None,
        provider: None,
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
