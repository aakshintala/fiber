//! The internal hub command (`docs/invocation.md`, "Commands and flags"):
//! hidden, free to change, and named nowhere in the docs. It reads
//! configuration, builds the session starter, and runs the hub.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;

use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use serde_json::Value;

use crate::{cli, settings};

/// Runs the internal hub command.
pub(crate) fn run(command: cli::HubCommands) -> i32 {
    match command {
        cli::HubCommands::Serve => serve(),
    }
}

fn serve() -> i32 {
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(error) => return fail(&error.to_string()),
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(error) => return fail(&format!("the current directory: {error}")),
    };
    let (_, project) = match ::cli::project_of(&home, &workspace) {
        Ok(found) => found,
        Err(error) => return fail(&error.message),
    };
    let config = match config::Config::load(config::Sources {
        home: home.clone(),
        workspace,
        project,
        overrides: Vec::new(),
    }) {
        Ok(config) => config,
        Err(error) => return fail(&error.to_string()),
    };
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(crate::clock::System);
    hub::serve(
        &home,
        settings::hub_idle_exit(&config),
        env!("CARGO_PKG_VERSION"),
        Arc::new(SpawnStarter),
        clock,
    )
}

fn fail(message: &str) -> i32 {
    eprintln!("fiber: {message}");
    1
}

/// Starts the internal session command: `<current_exe> session --id <id>
/// --workspace <workspace> [--model <model>]`, never `--prompt`. Whoever
/// starts a session generates its id and passes it on that command's line,
/// so the starter knows the id before the process runs and nothing is read
/// back (`docs/invocation.md`, "The hub").
struct SpawnStarter;

impl hub::Starter for SpawnStarter {
    fn start(
        &self,
        id: &SessionId,
        workspace: &Path,
        model: Option<&str>,
    ) -> std::io::Result<Box<dyn hub::Started>> {
        let exe = std::env::current_exe()?;
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
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        // The drain thread starts first, so a spawn failure strands
        // nothing: dropping `tx` ends its wait.
        let (tx, rx) = mpsc::channel::<Child>();
        let state = Arc::new(Mutex::new(State {
            exited: false,
            failure: None,
        }));
        let watch = Arc::clone(&state);
        thread::Builder::new()
            .name("hub-drain".to_owned())
            .spawn(move || {
                let Ok(child) = rx.recv() else {
                    return;
                };
                drain(child, &watch);
            })
            .map_err(|error| std::io::Error::new(error.kind(), format!("hub drain: {error}")))?;
        let child = command.spawn()?;
        match tx.send(child) {
            Ok(()) => Ok(Box::new(Spawned {
                id: id.clone(),
                state,
            })),
            Err(failed) => {
                let mut child = failed.0;
                match child.kill() {
                    Ok(()) | Err(_) => {}
                }
                match child.wait() {
                    Ok(_) | Err(_) => {}
                }
                Err(std::io::Error::other(format!(
                    "session {} never started draining",
                    id.0
                )))
            }
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
fn drain(mut child: Child, state: &Arc<Mutex<State>>) {
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
