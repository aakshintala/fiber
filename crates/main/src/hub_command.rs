//! `fiber hub` (`docs/invocation.md`, "Commands and flags"). `install`,
//! `uninstall`, `status`, `refresh`, `pair` and `token` run in the `cli` crate. The internal
//! `hub serve` is hidden, free to change, and named nowhere in the docs: it
//! builds the session starter and runs the hub, which reads configuration
//! once it holds the `run/` lock; a start failure prints one line on stderr.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use serde_json::Value;

use crate::{cli, settings};

/// Runs a `fiber hub` command.
pub(crate) fn run(
    command: cli::HubCommands,
    exe: Result<PathBuf, String>,
    clock: &dyn contract::clock::Clock,
) -> i32 {
    match command {
        cli::HubCommands::Add {
            name,
            address,
            default,
        } => ::cli::hub_add(&name, &address, default, clock),
        cli::HubCommands::Remove { name } => ::cli::hub_remove(&name),
        cli::HubCommands::Install { port } => ::cli::hub_install(port, &crate::clock::System),
        cli::HubCommands::Uninstall => ::cli::hub_uninstall(),
        cli::HubCommands::Status { json } => ::cli::hub_status(json, &crate::clock::System),
        cli::HubCommands::Refresh { hub } => {
            client_command(hub.as_deref(), &exe, clock, ::cli::hub_refresh)
        }
        cli::HubCommands::Pair {
            device,
            address,
            hub,
        } => client_command(hub.as_deref(), &exe, clock, |connect| {
            ::cli::hub_pair(&device, address.as_ref(), connect)
        }),
        cli::HubCommands::Token(cli::TokenCommands::List { hub }) => {
            client_command(hub.as_deref(), &exe, clock, ::cli::hub_token_list)
        }
        cli::HubCommands::Token(cli::TokenCommands::Revoke { device, hub }) => {
            client_command(hub.as_deref(), &exe, clock, |connect| {
                ::cli::hub_token_revoke(&device, connect)
            })
        }
        cli::HubCommands::Serve { installed } => serve(exe, mode(installed)),
    }
}

fn client_command(
    named: Option<&str>,
    exe: &Result<PathBuf, String>,
    clock: &dyn contract::clock::Clock,
    run: impl FnOnce(&mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>) -> i32,
) -> i32 {
    let (home, target) = match crate::hub_target(named) {
        Ok(target) => target,
        Err(error) => return fail(error),
    };
    run(&mut || crate::connect_hub(&home, &target, exe, clock))
}

/// `--installed` is the login service's hub; without it a client started
/// the hub.
fn mode(installed: bool) -> hub::Mode {
    if installed {
        hub::Mode::Installed
    } else {
        hub::Mode::OnDemand
    }
}

fn serve(exe: Result<PathBuf, String>, mode: hub::Mode) -> i32 {
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
    // The hub's inherited environment, read once: the login-shell capture
    // falls back to it, with `FIBER_HOME` set to the hub's own home.
    let environments = hub::Environments::new(std::env::vars_os().collect(), &home);
    finish(hub::serve(
        &home,
        mode,
        configure,
        env!("CARGO_PKG_VERSION"),
        Arc::new(SpawnStarter {
            exe,
            environments: Arc::clone(&environments),
        }),
        clock,
        environments,
        Arc::new(crate::configure::Seam::new(home.clone())),
    ))
}

/// The hub's `hub.idle_exit_ms` and `diagnostics.level`, from the
/// configuration read in `workspace`, the current directory. The port
/// comes from Fiber home's global `config.json` only: a project file's
/// `hub.port` is ignored (`docs/invocation.md`, "The hub").
fn configure(home: &Path, workspace: std::io::Result<PathBuf>) -> Result<hub::Settings, Failure> {
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
    Ok(hub::Settings {
        idle_exit: settings::hub_idle_exit(&config),
        level: if config::diagnostics_debug(&config) {
            log::diag::Level::Debug
        } else {
            log::diag::Level::Info
        },
        port: port(home, &config)?,
    })
}

/// The port an installed hub also listens on: `Some` when Fiber home's
/// global `config.json` holds a `hub.port`, with the web page origins
/// from the loaded configuration, else `None`.
fn port(home: &Path, config: &config::Config) -> Result<Option<hub::PortSettings>, Failure> {
    let number = config::get_global(home, "hub.port")
        .map_err(|error| failure(error.code(), error.to_string()))?
        .and_then(|value| value.as_u64())
        .and_then(|number| u16::try_from(number).ok());
    let Some(number) = number else {
        return Ok(None);
    };
    let allowed_origins = config
        .get("hub.allowed_origins", None)
        .and_then(|(value, _)| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|origin| origin.as_str().map(str::to_owned))
        .collect();
    Ok(Some(hub::PortSettings {
        number,
        allowed_origins,
    }))
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
        retry_after_ms: None,
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

/// Starts the internal session command: `<exe> session --id <id>
/// --workspace <workspace> [--model <model>]`, never `--prompt`, or with
/// `--resume` for a session the log holds. Whoever starts a session
/// generates its id and passes it on that command's line, so the starter
/// knows the id before the process runs and nothing is read back
/// (`docs/invocation.md`, "The hub").
struct SpawnStarter {
    /// This process's path, read once when the hub starts.
    exe: PathBuf,
    /// The environment every session starts with: the login-shell
    /// capture's publish, once the first capture ends.
    environments: Arc<hub::Environments>,
}

impl hub::Starter for SpawnStarter {
    fn start(
        &self,
        id: &SessionId,
        workspace: &Path,
        model: Option<&str>,
        overrides: &[&str],
        worktree: bool,
    ) -> std::io::Result<Box<dyn hub::Started>> {
        let mut command = start_command(&self.exe, id, workspace, model, overrides, worktree);
        apply(&mut command, &self.environments.get());
        spawn(id, command)
    }

    fn resume(
        &self,
        id: &SessionId,
        workspace: &Path,
        credential: Option<&str>,
    ) -> std::io::Result<Box<dyn hub::Started>> {
        let mut command = session_command(&self.exe, id, workspace, None, false, true, None);
        // One argument, so an empty or leading-hyphen label from the
        // wire cannot be consumed as another flag.
        if let Some(label) = credential {
            command.arg(format!("--credential={label}"));
        }
        apply(&mut command, &self.environments.get());
        spawn(id, command)
    }

    fn rewind(
        &self,
        id: &SessionId,
        workspace: &Path,
        from: &SessionId,
    ) -> std::io::Result<Box<dyn hub::Started>> {
        let mut command = session_command(&self.exe, id, workspace, None, false, false, Some(from));
        apply(&mut command, &self.environments.get());
        spawn(id, command)
    }
}

/// The hub's current environment onto the session command: exactly its
/// variables, and `--login-shell` exactly when they came from the
/// login-shell capture (`docs/invocation.md`, "A session's environment").
fn apply(command: &mut Command, environment: &hub::Environment) {
    command.env_clear();
    command.envs(environment.vars.iter().map(|(name, value)| (name, value)));
    if environment.source == contract::events::VariablesSource::LoginShell {
        command.arg("--login-shell");
    }
}

/// The session command a `start` spawns: `session_command` with one `-c
/// <override>` per override, in order. An override is passed unchanged,
/// as one value of `-c`, so it never becomes another flag.
fn start_command(
    exe: &Path,
    id: &SessionId,
    workspace: &Path,
    model: Option<&str>,
    overrides: &[&str],
    worktree: bool,
) -> Command {
    let mut command = session_command(exe, id, workspace, model, worktree, false, None);
    for override_text in overrides {
        command.arg("-c").arg(override_text);
    }
    command
}

/// `exe session --id <id> --workspace <workspace>`, with `--model` when
/// one is named, `--worktree` for a new worktree, `--resume` for a resume
/// and `--rewound-from` for a rewind's new session, in its own process
/// group with stdout piped for the drain.
fn session_command(
    exe: &Path,
    id: &SessionId,
    workspace: &Path,
    model: Option<&str>,
    worktree: bool,
    resume: bool,
    rewound_from: Option<&SessionId>,
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
    if worktree {
        command.arg("--worktree");
    }
    if resume {
        command.arg("--resume");
    }
    if let Some(from) = rewound_from {
        command.arg("--rewound-from").arg(&from.0);
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
    let shared: Arc<(Mutex<State>, Condvar)> = Arc::new((
        Mutex::new(State {
            exited: false,
            failure: None,
        }),
        Condvar::new(),
    ));
    let child = Arc::new(Mutex::new(child));
    let watch = Arc::clone(&shared);
    match thread::Builder::new().name("hub-drain".to_owned()).spawn({
        let child = Arc::clone(&child);
        move || drain(&child, &watch)
    }) {
        Ok(_) => Ok(Box::new(Spawned {
            id: id.clone(),
            state: shared,
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
    /// `drain` notifies the `Condvar` once it sets `State::exited`.
    state: Arc<(Mutex<State>, Condvar)>,
}

struct State {
    exited: bool,
    /// The `fiber_exited` line printed last on stdout, when it parsed.
    failure: Option<Failure>,
}

impl hub::Started for Spawned {
    fn exited(&self) -> Option<Failure> {
        let state = lock(&self.state.0);
        if !state.exited {
            return None;
        }
        Some(state.failure.clone().unwrap_or_else(|| Failure {
            code: ErrorCode::IoFailed,
            message: format!("Session {} exited without a verdict.", self.id.0),
            retry_after_ms: None,
            provider: None,
        }))
    }

    fn wait_exited(&self) -> Option<Failure> {
        let mut state = lock(&self.state.0);
        while !state.exited {
            state = self
                .state
                .1
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(state);
        self.exited()
    }
}

/// Reads the session's stdout to EOF, keeping the `fiber_exited` line
/// printed last, then reaps the child.
fn drain(child: &Arc<Mutex<Child>>, shared: &Arc<(Mutex<State>, Condvar)>) {
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
                        lock(&shared.0).failure = Some(failure);
                    }
                }
            }
        }
    }
    match child.wait() {
        Ok(_) | Err(_) => {}
    }
    lock(&shared.0).exited = true;
    shared.1.notify_all();
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
        retry_after_ms: None,
        provider: None,
    })
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "hub_command_tests.rs"]
mod tests;
