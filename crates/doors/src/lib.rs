//! The doors (`docs/invocation.md`): `fiber ask`, whose argument or stdin is
//! the prompt and whose stdout is the event stream, and the internal session
//! command it runs, one process per session ("Processes"). The session
//! command owns the process boundary: the session's socket and its
//! `fiber_started` and `fiber_exited` lines.
//!
//! `main` builds the session's parts; this crate never sees a provider or
//! the loop (`docs/architecture.md`, "The call rules").

mod attach;
mod client;
pub mod hub;
mod session;
mod shell;
mod signals;
mod socket;

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use contract::shapes::Failure;
use contract::{ErrorCode, PreSessionExit};

pub use attach::attach;
pub use session::Session;
pub use signals::{Signals, signal_code};

/// A failure with Fiber's own sentence and nothing from a provider.
pub fn failure(code: ErrorCode, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
        retry_after: None,
        provider: None,
    }
}

/// The exit code a failure ends the process with: 2 for `usage`, 1 for any
/// other (`docs/errors.md`, "Before a session exists").
pub fn exit_code(failure: &Failure) -> i32 {
    if failure.code == ErrorCode::Usage {
        2
    } else {
        1
    }
}

/// `fiber ask`'s prompt (`docs/invocation.md`, "Getting a prompt in").
///
/// The source is the arguments alone. Stdin is read only when `dash` is set,
/// or when `arg` is absent and stdin is not a terminal. A whitespace-only
/// part is dropped. When both parts remain, the prompt is the argument, a
/// newline, then stdin.
pub fn prompt(
    arg: Option<String>,
    dash: bool,
    stdin: &mut dyn Read,
    terminal: bool,
) -> Result<String, Failure> {
    let supplied = arg.is_some();
    let kept = arg.filter(|text| !text.trim().is_empty());
    // An argument and no `-` never reads stdin, even when the argument is blank.
    if !dash && supplied {
        return kept.ok_or_else(no_prompt);
    }
    if !dash && terminal {
        return Err(no_prompt());
    }
    let mut piped = String::new();
    stdin.read_to_string(&mut piped).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidData {
            failure(ErrorCode::Usage, "stdin is not UTF-8 text.")
        } else {
            failure(ErrorCode::IoFailed, format!("stdin could not be read: {e}"))
        }
    })?;
    let piped = (!piped.trim().is_empty()).then_some(piped);
    match (kept, piped) {
        (Some(prompt), Some(stdin)) => Ok(format!("{prompt}\n{stdin}")),
        (Some(prompt), None) | (None, Some(prompt)) => Ok(prompt),
        (None, None) => Err(no_prompt()),
    }
}

fn no_prompt() -> Failure {
    failure(
        ErrorCode::Usage,
        "No prompt. Run `fiber ask \"<prompt>\"` or `fiber ask < <file>`.",
    )
}

/// What `fiber extension install` shows before it installs (`docs/extensions.md`,
/// "What an install shows"): what the manifest and the files tell. Tools,
/// hooks, watchers and commands are not shown until `docs/extensions.md`
/// settles how Fiber learns them before the extension runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSummary {
    /// The extension's name.
    pub name: String,
    /// Where it is installed from.
    pub source: String,
    /// Its version.
    pub version: String,
    /// For an update, what changed since the installed commit.
    pub changes: Option<String>,
    /// Each provider it registers, with its models' base URLs.
    pub providers: Vec<(String, Vec<String>)>,
    /// The program a process extension runs, with its arguments.
    pub process: Option<String>,
    /// Its install step.
    pub install_step: Option<String>,
    /// What it carries, one line each: skills, prompt templates, themes,
    /// binaries and the TUI extension.
    pub carries: Vec<String>,
    /// Its files as staged, which are what will be installed.
    pub staged: PathBuf,
}

/// Whether `fiber extension install` goes ahead (`docs/extensions.md`, "Installing"):
/// in a terminal it writes each of `summaries` to `out` and asks once, and
/// only `y` or `yes` goes ahead; `s` shows every file and asks again.
/// Without a terminal it goes ahead without asking.
pub fn install_approved(
    summaries: &[InstallSummary],
    terminal: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<bool, Failure> {
    if !terminal {
        return Ok(true);
    }
    let io = |e: io::Error| failure(ErrorCode::IoFailed, format!("the terminal: {e}"));
    let mut text = String::new();
    for summary in summaries {
        let verb = if summary.changes.is_some() {
            "Update"
        } else {
            "Install"
        };
        text.push_str(&format!(
            "{verb} {} from {}\nVersion {}\n",
            summary.name, summary.source, summary.version
        ));
        if let Some(changes) = &summary.changes {
            text.push_str(&format!("Changes since the installed commit:\n{changes}"));
            if !changes.ends_with('\n') {
                text.push('\n');
            }
        }
        if summary.providers.is_empty() {
            text.push_str("It registers no provider.\n");
        }
        for (provider, urls) in &summary.providers {
            text.push_str(&format!("Provider {provider}: {}\n", urls.join(", ")));
        }
        if let Some(process) = &summary.process {
            text.push_str(&format!("Runs the program: {process}\n"));
        }
        if let Some(step) = &summary.install_step {
            text.push_str(&format!(
                "Install step, run now and at every update: {step}\n\
                 Its dependencies' own install scripts run too.\n"
            ));
        }
        for line in &summary.carries {
            text.push_str(&format!("Carries {line}\n"));
        }
    }
    out.write_all(text.as_bytes()).map_err(io)?;
    loop {
        out.write_all(b"Go ahead? [y/N/s to show the full source] ")
            .and_then(|()| out.flush())
            .map_err(io)?;
        let mut answer = String::new();
        input.read_line(&mut answer).map_err(io)?;
        match answer.trim() {
            "s" | "S" => {
                for summary in summaries {
                    show_source(&summary.name, &summary.staged, out).map_err(io)?;
                }
            }
            "y" | "Y" | "yes" => return Ok(true),
            _ => return Ok(false),
        }
    }
}

/// Writes every file under `dir`, in path order, each after its relative
/// path. A file that is not text is named with its size.
fn show_source(name: &str, dir: &Path, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "=== {name}")?;
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    for file in files {
        let shown = file
            .strip_prefix(dir)
            .unwrap_or(&file)
            .display()
            .to_string();
        match std::fs::read(&file).map(String::from_utf8) {
            Ok(Ok(text)) => {
                writeln!(out, "--- {shown}")?;
                out.write_all(text.as_bytes())?;
                if !text.ends_with('\n') {
                    writeln!(out)?;
                }
            }
            Ok(Err(e)) => writeln!(out, "--- {shown} ({} bytes, not text)", e.as_bytes().len())?,
            Err(_) => writeln!(out, "--- {shown} (a link, or unreadable)")?,
        }
    }
    Ok(())
}

/// Whether `fiber extension remove` goes ahead: in a terminal it lists what it will
/// delete, the extensions and their data and settings, and asks; without a
/// terminal it goes ahead (`docs/state.md`, "Extension data").
pub fn remove_approved(
    names: &[String],
    data: &[PathBuf],
    terminal: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<bool, Failure> {
    if !terminal {
        return Ok(true);
    }
    let io = |e: io::Error| failure(ErrorCode::IoFailed, format!("the terminal: {e}"));
    let mut text = String::new();
    for name in names {
        text.push_str(&format!("Remove {name}\n"));
    }
    for path in data {
        text.push_str(&format!("Delete {}\n", path.display()));
    }
    text.push_str("Go ahead? [y/N] ");
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(io)?;
    let mut answer = String::new();
    input.read_line(&mut answer).map_err(io)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// Ends a process that failed before any session existed: the
/// `fiber_exited` line with no `session_id` on `out`, and one sentence on
/// `err` (`docs/errors.md`, "Before a session exists"). Returns the exit
/// code.
pub fn exit_before_session(failure: Failure, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let code = exit_code(&failure);
    let message = failure.message.clone();
    // Nothing is left to report a failed write to.
    if let Ok(mut line) = serde_json::to_vec(&PreSessionExit::new(code, failure)) {
        line.push(b'\n');
        out.write_all(&line)
            .and_then(|()| out.flush())
            .unwrap_or(());
    }
    writeln!(err, "fiber: {message}").unwrap_or(());
    code
}

/// A new id from random bytes, such as a session's or a command's
/// (`docs/events.md`, "Identity and ordering"). `RandomState` seeds its keys
/// from the operating system's randomness.
pub fn mint(prefix: &str) -> String {
    format!("{prefix}{:016x}", RandomState::new().hash_one(()))
}

/// The project's identity path (`docs/state.md`, "Projects"): git's shared
/// directory when `launch` is inside a repository, otherwise `launch`
/// itself, symlinks resolved.
pub fn project(launch: &Path) -> PathBuf {
    let common = Command::new("git")
        .arg("-C")
        .arg(launch)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let identity = match common {
        Ok(out) if out.status.success() => {
            PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'))
        }
        // Not a repository, or no git: the launch directory is the project.
        Ok(_) | Err(_) => launch.to_path_buf(),
    };
    identity.canonicalize().unwrap_or(identity)
}
