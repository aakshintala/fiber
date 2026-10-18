//! The shell tool (`docs/tools.md`, "Shell"): run a command, bound its output,
//! and stop its process group on timeout or cancel.

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::diag::Timing;
use contract::emit::Emit;
use contract::jobs::Jobs;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Failure, Process};
use contract::tool::{Bound, Cancel, Effects, Output, Tool};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::tool_util::{cancelled_before, failed, failure};

mod background;

mod command;
mod drive;
mod moved;
mod process_group;
mod spawn;

mod monitor;

mod output;

mod prelude;

mod spill;

mod tty;

mod read_only;

mod classify;
mod matches;

mod sed;

use classify::classify;
use command::{Finished, StopKind};

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

const CANCELLED: &str = "Cancelled and stopped.";
const INDETERMINATE: &str = "The command was stopped, and Fiber cannot tell whether it completed.";
const HELD_OPEN: &str = "Output was still held open.";

/// One shell tool. Each call has its own process; concurrent calls share nothing mutable.
#[derive(Clone)]
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
    /// The session's `artifacts/`, when the call spills its output past
    /// 1 MiB into its artifact file (`docs/tools.md`, "Result and output").
    artifacts: Option<PathBuf>,
    /// Times a background start as `jobs.start`, when installed by
    /// [`Shell::with_timing`]. The stamp is read before the spawn work;
    /// the guard is created and dropped once the job runs.
    timing: Option<Timing>,
    /// The person's `shell.read_only` entries, shared with every clone:
    /// replacing the contents changes the next [`Tool::effects`] answer
    /// of this shell and of each clone, including a capped one. The lock
    /// is taken in `effects` only; a poisoned lock keeps its contents.
    #[allow(
        clippy::type_complexity,
        reason = "the shared handle spells its contents; main's settings::ShellReadOnly names the same type"
    )]
    read_only: Arc<Mutex<Vec<(String, Vec<String>)>>>,
    /// The held ends of a result: the first `start` and the last `end` bytes.
    bound: Bound,
    /// A test's writer around a spill's file, if any.
    spill_wrap: Option<spill::Wrap>,
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
            artifacts: None,
            timing: None,
            read_only: Arc::new(Mutex::new(Vec::new())),
            bound: Bound {
                start: 8192,
                end: 8192,
            },
            spill_wrap: None,
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

    /// Spills a foreground call's output past 1 MiB into its artifact file
    /// in `dir` (`docs/tools.md`, "Result and output"). The directory is
    /// stored absolute, as given when that fails.
    pub fn with_artifacts(self, dir: PathBuf) -> Self {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        Self {
            artifacts: Some(dir),
            ..self
        }
    }

    /// Times a background start as `jobs.start`, from just before its
    /// spawn until the job is recorded and its own thread runs. A call
    /// that never moves writes nothing.
    #[must_use]
    pub fn with_timing(self, timing: Timing) -> Self {
        Self {
            timing: Some(timing),
            ..self
        }
    }

    /// Answers the shell classifier from the person's `shell.read_only`
    /// entries beside the built-in list (`docs/tools.md`, "Other
    /// command-line tools"). The handle is shared: a reload that replaces
    /// its contents reaches this shell and every clone of it. An entry
    /// whose basename is built in is never consulted.
    #[allow(
        clippy::type_complexity,
        reason = "the shared handle spells its contents; main's settings::ShellReadOnly names the same type"
    )]
    #[must_use]
    pub fn with_read_only(self, entries: Arc<Mutex<Vec<(String, Vec<String>)>>>) -> Self {
        Self {
            read_only: entries,
            ..self
        }
    }

    /// Wraps a spill's file in a test's own writer.
    #[cfg(test)]
    pub(crate) fn with_spill_writer(self, wrap: spill::Wrap) -> Self {
        Self {
            spill_wrap: Some(wrap),
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
            Ok(parsed) => {
                let entries = self
                    .read_only
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                Ok(classify(&parsed.command, &parsed.workdir, &entries))
            }
            Err(_) => Ok(classify::executes(None, None)),
        }
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, emit: &dyn Emit) -> Output {
        if cancel.is_cancelled() {
            return cancelled_before();
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
        let spill = self.artifacts.clone().map(|dir| spill::Config {
            dir,
            bound: self.bound,
            wrap: self.spill_wrap.clone(),
            name: None,
        });
        // The stamp covers the spawn and adoption setup below, never a
        // foreground call's earlier life: only the three background
        // policies stamp here, and `take` stamps a moved foreground call
        // at its own entry. No clock is read without an installed timing.
        let background = matches!(
            policy,
            command::MovePolicy::Background
                | command::MovePolicy::Terminal
                | command::MovePolicy::Monitor
        );
        let started = if background && self.timing.is_some() {
            Some(self.clock.now())
        } else {
            None
        };
        let matches = if self.search.is_some() && parsed.mode == Mode::Foreground {
            matches::MatchDir::create()
        } else {
            None
        };
        let side = matches.as_ref().map(matches::MatchDir::file);
        let mut output = match command::execute(
            program,
            &command,
            &parsed.workdir,
            Duration::from_millis(parsed.timeout_ms),
            self.clock.as_ref(),
            cancel,
            emit,
            policy,
            self.jobs.as_deref(),
            spill,
            side.as_deref(),
        ) {
            Ok(command::Ran::Finished(finished)) => {
                from_spawn(Ok(finished), limit, &parsed.command)
            }
            Ok(command::Ran::Moved(moved)) => match &self.jobs {
                Some(jobs) => background::take(
                    *moved,
                    Arc::clone(jobs),
                    Arc::clone(&self.clock),
                    &parsed.command,
                    limit,
                    matches!(parsed.mode, Mode::Terminal { .. }),
                    cancel,
                    emit,
                    self.timing.as_ref(),
                    started,
                ),
                None => {
                    let finished = moved.resume(self.clock.as_ref(), cancel, emit);
                    from_spawn(Ok(finished), limit, &parsed.command)
                }
            },
            Err(err) => from_spawn(Err(err), limit, &parsed.command),
        };
        if output.process.is_some()
            && classify::one_grep(&parsed.command)
            && let Some(side) = side
            && let Some(matches) = matches::read(&side, crate::search::matches::LIMIT)
            && let Some(Value::Object(details)) = output.details.as_mut()
        {
            details.insert("matches".into(), matches);
        }
        output
    }

    fn bound(&self) -> Bound {
        self.bound
    }

    fn with_cap(&self, cap: usize) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            bound: Bound {
                start: cap.saturating_sub(cap / 2),
                end: cap / 2,
            },
            ..self.clone()
        }))
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("shell")
    }
}

/// The call's arguments, checked against the schema before the call runs.
#[derive(Debug, Deserialize)]
struct Args {
    command: String,
    workdir: Option<String>,
    timeout_ms: Option<u64>,
    deadline_ms: Option<u64>,
    #[serde(default)]
    run_in_background: bool,
    #[serde(default)]
    tty: bool,
    #[serde(default)]
    monitor: bool,
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
    let args: Args = crate::tool_util::arguments(arguments)?;
    let command = args.command;
    let workdir = workdir(args.workdir.as_deref(), workspace)?;
    let given_timeout = args.timeout_ms;
    let given_deadline = args.deadline_ms;
    let run_in_background = args.run_in_background;
    let tty = args.tty;
    let monitor = args.monitor;
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

fn workdir(workdir: Option<&str>, workspace: &Path) -> Result<PathBuf, String> {
    let path = match workdir {
        None => workspace.to_path_buf(),
        Some(raw) => {
            let given = Path::new(raw);
            if given.is_absolute() {
                given.to_path_buf()
            } else {
                workspace.join(given)
            }
        }
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

fn from_spawn(result: Result<Finished, std::io::Error>, limit: Limit, command: &str) -> Output {
    match result {
        Ok(finished) => assemble(limit, finished, None, Some(command)),
        Err(err) => {
            let mut output = failed(
                ErrorCode::ToolError,
                format!("The command could not be started: {err}."),
            );
            output.details = Some(details(command, None));
            output
        }
    }
}

fn details(command: &str, process: Option<&Process>) -> Value {
    let mut details = Map::new();
    details.insert("command".into(), json!(command));
    if let Some(process) = process {
        if let Some(code) = process.exit_code {
            details.insert("exit_code".into(), json!(code));
        } else if let Some(signal) = &process.signal {
            details.insert("signal".into(), json!(signal));
        }
    }
    Value::Object(details)
}

/// The result of a finished command: `extra`, when the job could not be
/// opened, goes before the closing line, for a whole and a spilled output
/// alike. A spilled output carries no text parts: the loop takes its text
/// from the spill alone.
pub(crate) fn assemble(
    limit: Limit,
    finished: Finished,
    extra: Option<&str>,
    command: Option<&str>,
) -> Output {
    let mut output = assemble_output(limit, finished, extra);
    output.details = command.map(|command| details(command, output.process.as_ref()));
    output
}

fn assemble_output(limit: Limit, finished: Finished, extra: Option<&str>) -> Output {
    let timed_out = finished.stop == Some(StopKind::Timeout);
    if finished.indeterminate {
        let process = process_of(&finished, timed_out);
        return end(
            finished,
            extra,
            INDETERMINATE,
            Some(failure(ErrorCode::Indeterminate, INDETERMINATE.to_owned())),
            process,
            false,
        );
    }
    if finished.stop == Some(StopKind::Timeout) {
        let line = limit.line();
        let error = Some(failure(ErrorCode::Timeout, line.clone()));
        let process = process_of(&finished, true);
        return end(finished, extra, &line, error, process, false);
    }
    if finished.stop == Some(StopKind::Cancel) {
        let process = process_of(&finished, false);
        return end(finished, extra, CANCELLED, None, process, false);
    }
    let Some(status) = finished.status else {
        return failed(
            ErrorCode::ToolError,
            "The command's process could not be reaped.".to_owned(),
        );
    };
    let (exit_code, signal, line) = observed(status);
    let held_open = finished.held_open;
    let sent_signal = finished.sent_signal;
    let error = if signal.is_some() && !sent_signal {
        Some(failure(ErrorCode::Signal, line.clone()))
    } else if exit_code.is_some_and(|code| code != 0) {
        Some(failure(ErrorCode::NonzeroExit, line.clone()))
    } else {
        None
    };
    end(
        finished,
        extra,
        &line,
        error,
        Process {
            exit_code,
            signal,
            timed_out: false,
        },
        held_open,
    )
}

/// `finished` ended with `line` as its closing line and `error`: its whole
/// output gets the line as text, its spilled output as the file's end.
/// `held_open` appends its line to a whole output only: it names bytes the
/// file does not hold.
fn end(
    finished: Finished,
    extra: Option<&str>,
    line: &str,
    error: Option<Failure>,
    process: Process,
    held_open: bool,
) -> Output {
    match finished.output {
        command::Kept::Whole(output) => {
            let mut text = text_of(&output, &closing(extra, line));
            if held_open {
                text.push_str(HELD_OPEN);
                text.push('\n');
            }
            with_process(text, error, process)
        }
        command::Kept::Spilled(spill) => {
            let spilled = spill.close(extra, line);
            Output {
                error,
                process: Some(process),
                spilled: Some(spilled),
                ..Output::default()
            }
        }
    }
}

/// The closing lines as text: `extra` before `line`.
fn closing(extra: Option<&str>, line: &str) -> String {
    match extra {
        Some(extra) => format!("{extra}\n{line}"),
        None => line.to_owned(),
    }
}

fn observed(status: ExitStatus) -> (Option<i32>, Option<String>, String) {
    if let Some(number) = status.signal() {
        let name = support::group::signal_name(number);
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

fn text_of(output: &[u8], line: &str) -> String {
    let mut text = String::from_utf8_lossy(output).into_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    text
}

fn with_process(text: String, error: Option<Failure>, process: Process) -> Output {
    Output {
        content: vec![ContentPart::Text { text }],
        error,
        process: Some(process),
        ..Output::default()
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
