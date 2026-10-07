//! The internal hub command (`docs/invocation.md`, "Commands and flags"):
//! hidden, free to change, and named nowhere in the docs. It builds the
//! session starter and runs the hub, which reads configuration once it holds
//! the `run/` lock; a start failure prints one line on stderr.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use serde_json::Value;

use crate::{cli, settings};

/// Runs the internal hub command.
pub(crate) fn run(command: cli::HubCommands, exe: Result<PathBuf, String>) -> i32 {
    match command {
        cli::HubCommands::Serve => serve(exe),
    }
}

fn serve(exe: Result<PathBuf, String>) -> i32 {
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(error) => return fail(failure(error.code(), error.to_string())),
    };
    let exe = match exe {
        Ok(exe) => exe,
        Err(message) => return fail(failure(ErrorCode::IoFailed, message)),
    };
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(crate::clock::System);
    let configure = {
        let home = home.clone();
        move || configure(&home, std::env::current_dir())
    };
    finish(hub::serve(
        &home,
        configure,
        env!("CARGO_PKG_VERSION"),
        Arc::new(SpawnStarter { exe }),
        clock,
    ))
}

/// The hub's `hub.idle_exit_ms`, from the configuration read in
/// `workspace`, the current directory.
fn configure(home: &Path, workspace: std::io::Result<PathBuf>) -> Result<Duration, Failure> {
    let workspace = workspace.map_err(|error| {
        failure(
            ErrorCode::IoFailed,
            format!("the current directory: {error}"),
        )
    })?;
    let (_, project) = ::cli::project_of(home, &workspace)?;
    let config = config::Config::load(config::Sources {
        home: home.to_path_buf(),
        workspace,
        project,
        overrides: Vec::new(),
    })
    .map_err(|error| failure(error.code(), error.to_string()))?;
    Ok(settings::hub_idle_exit(&config))
}

/// The hub's exit code, printing a start failure the way every command
/// does.
fn finish(served: Result<i32, hub::StartError>) -> i32 {
    match served {
        Ok(code) => code,
        Err(error) => fail(failure(error.code(), error.to_string())),
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

/// Prints `fiber: <message>` on stderr and gives the failure's exit code:
/// 2 for `usage`, otherwise 1.
fn fail(failure: Failure) -> i32 {
    // A closed stderr leaves nobody to tell.
    writeln!(std::io::stderr(), "fiber: {}", failure.message).unwrap_or(());
    doors::exit_code(&failure)
}

/// Starts the internal session command: `<current_exe> session --id <id>
/// --workspace <workspace> [--model <model>]`, never `--prompt`, or with
/// `--resume` for a session the log holds. Whoever starts a session
/// generates its id and passes it on that command's line, so the starter
/// knows the id before the process runs and nothing is read back
/// (`docs/invocation.md`, "The hub").
struct SpawnStarter {
    /// This process's path, read once when the hub starts.
    exe: PathBuf,
}

impl hub::Starter for SpawnStarter {
    fn start(
        &self,
        id: &SessionId,
        workspace: &Path,
        model: Option<&str>,
    ) -> std::io::Result<Box<dyn hub::Started>> {
        spawn(id, session_command(&self.exe, id, workspace, model, false))
    }

    fn resume(&self, id: &SessionId, workspace: &Path) -> std::io::Result<Box<dyn hub::Started>> {
        spawn(id, session_command(&self.exe, id, workspace, None, true))
    }
}

/// `exe session --id <id> --workspace <workspace>`, with `--model` when
/// one is named and `--resume` for a resume, in its own process group with
/// stdout piped for the drain.
fn session_command(
    exe: &Path,
    id: &SessionId,
    workspace: &Path,
    model: Option<&str>,
    resume: bool,
) -> Command {
    let mut command = Command::new(exe);
    command
        .arg("session")
        .arg("--id")
        .arg(&id.0)
        .arg("--workspace")
        .arg(workspace);
    if let Some(model) = model {
        command.arg("--model").arg(model);
    }
    if resume {
        command.arg("--resume");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

/// Spawns `command` for session `id` and drains its stdout on a thread.
fn spawn(id: &SessionId, mut command: Command) -> std::io::Result<Box<dyn hub::Started>> {
    // The child starts first, then the drain thread takes it: a thread
    // that never starts leaves the child behind, so it is killed and
    // reaped here.
    let child = command.spawn()?;
    let state = Arc::new(Mutex::new(State {
        exited: false,
        failure: None,
    }));
    let child = Arc::new(Mutex::new(child));
    let watch = Arc::clone(&state);
    match thread::Builder::new().name("hub-drain".to_owned()).spawn({
        let child = Arc::clone(&child);
        move || drain(&child, &watch)
    }) {
        Ok(_) => Ok(Box::new(Spawned {
            id: id.clone(),
            state,
        })),
        Err(error) => {
            let mut child = lock(&child);
            match child.kill() {
                Ok(()) | Err(_) => {}
            }
            match child.wait() {
                Ok(_) | Err(_) => {}
            }
            Err(std::io::Error::new(
                error.kind(),
                format!("hub drain: {error}"),
            ))
        }
    }
}

/// A session process the hub started: its stdout drained to EOF on a thread
/// that then reaps it, so no zombie. Closing the hub's end of the pipe ends
/// the drain; the session keeps running.
struct Spawned {
    id: SessionId,
    state: Arc<Mutex<State>>,
}

struct State {
    exited: bool,
    /// The `fiber_exited` line printed last on stdout, when it parsed.
    failure: Option<Failure>,
}

impl hub::Started for Spawned {
    fn exited(&self) -> Option<Failure> {
        let state = lock(&self.state);
        if !state.exited {
            return None;
        }
        Some(state.failure.clone().unwrap_or_else(|| Failure {
            code: ErrorCode::IoFailed,
            message: format!("Session {} exited without a verdict.", self.id.0),
            retry_after: None,
            provider: None,
        }))
    }
}

/// Reads the session's stdout to EOF, keeping the `fiber_exited` line
/// printed last, then reaps the child.
fn drain(child: &Arc<Mutex<Child>>, state: &Arc<Mutex<State>>) {
    let mut child = lock(child);
    if let Some(stdout) = child.stdout.take() {
        let mut read = BufReader::new(stdout);
        let mut buf = String::new();
        loop {
            buf.clear();
            match read.read_line(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Some(failure) = fiber_exited_error(&buf) {
                        lock(state).failure = Some(failure);
                    }
                }
            }
        }
    }
    match child.wait() {
        Ok(_) | Err(_) => {}
    }
    lock(state).exited = true;
}

/// The `code` and `message` of a `fiber_exited` line's `payload.error`.
fn fiber_exited_error(line: &str) -> Option<Failure> {
    let line: Value = serde_json::from_str(line).ok()?;
    if line.get("kind")?.as_str()? != "fiber_exited" {
        return None;
    }
    let error = line.get("payload")?.get("error")?;
    let code: ErrorCode = serde_json::from_value(error.get("code")?.clone()).ok()?;
    let message = error.get("message")?.as_str()?.to_owned();
    Some(Failure {
        code,
        message,
        retry_after: None,
        provider: None,
    })
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "hub_command_tests.rs"]
mod tests;
