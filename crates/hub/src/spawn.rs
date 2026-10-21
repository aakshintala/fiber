//! Starts the hub's session processes: the internal session command
//! runs in its own process group with stdout piped for the drain.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;

use contract::session_argv::SessionArgv;
use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use serde_json::Value;

use crate::{Environment, Environments, Started, Starter};

/// Starts the internal session command: `<exe> session --id <id>
/// --workspace <workspace> [--model <model>]`, never `--prompt`, or with
/// `--resume` for a session the log holds. Whoever starts a session
/// generates its id and passes it on that command's line, so the starter
/// knows the id before the process runs and nothing is read back
/// (`docs/invocation.md`, "The hub").
pub struct SpawnStarter {
    /// This process's path, read once when the hub starts.
    exe: PathBuf,
    /// The environment every session starts with: the login-shell
    /// capture's publish, once the first capture ends.
    environments: Arc<Environments>,
}

impl SpawnStarter {
    /// Starts sessions with `exe`, and every session's environment from
    /// `environments`.
    #[must_use]
    pub fn new(exe: PathBuf, environments: Arc<Environments>) -> Self {
        Self { exe, environments }
    }
}

impl Starter for SpawnStarter {
    fn start(
        &self,
        id: &SessionId,
        workspace: &Path,
        model: Option<&str>,
        overrides: &[&str],
        worktree: bool,
        continues: Option<&SessionId>,
    ) -> std::io::Result<Box<dyn Started>> {
        let environment = self.environments.get();
        let mut command = start_command(
            &self.exe,
            id,
            workspace,
            model,
            overrides,
            worktree,
            continues,
            login_shell(&environment),
        );
        apply(&mut command, &environment);
        spawn(id, command)
    }

    fn resume(
        &self,
        id: &SessionId,
        workspace: &Path,
        credential: Option<&str>,
    ) -> std::io::Result<Box<dyn Started>> {
        // A resume whose launch target is gone cannot bind: refuse before
        // spawning, naming the path, so the hub's wrap names it too. Only
        // a missing target refuses, as the hub's launch choice does.
        if crate::resume::missing(workspace) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} no longer exists", workspace.display()),
            ));
        }
        let environment = self.environments.get();
        let args = SessionArgv {
            id: id.clone(),
            workspace: workspace.to_path_buf(),
            resume: true,
            credential: credential.map(str::to_owned),
            login_shell: login_shell(&environment),
            ..SessionArgv::default()
        };
        let mut command = session_command_from(&self.exe, args);
        apply(&mut command, &environment);
        spawn(id, command)
    }

    fn rewind(
        &self,
        id: &SessionId,
        workspace: &Path,
        from: &SessionId,
    ) -> std::io::Result<Box<dyn Started>> {
        let environment = self.environments.get();
        let mut command = session_command(
            &self.exe,
            id,
            workspace,
            None,
            false,
            false,
            Some(from),
            None,
            login_shell(&environment),
        );
        apply(&mut command, &environment);
        spawn(id, command)
    }
}

/// Marks a hub-spawned session's process: set beside the startup snapshot
/// in [`apply`], so the session knows its process already holds its fixed
/// environment (`docs/invocation.md`, "A session's environment"). The
/// value is the names-only `"1"`, never a secret. `add_lua`
/// (`crates/main/src/lua_providers.rs`) keys off its presence: present →
/// the process environment is the snapshot, and no hub query runs;
/// absent → an in-process `fiber ask`, which queries the hub so the
/// session agrees with `fiber models` (issue #2909).
pub const HUB_SESSION_MARKER: &str = "FIBER_HUB_SESSION";

/// The hub's current environment onto the session command: exactly its
/// variables (`docs/invocation.md`, "A session's environment"), with the
/// session marker riding beside the snapshot it marks.
fn apply(command: &mut Command, environment: &Environment) {
    command.env_clear();
    command.envs(environment.vars.iter().map(|(name, value)| (name, value)));
    command.env(HUB_SESSION_MARKER, "1");
}

/// Whether the session command carries `--login-shell`: only when the
/// environment came from the login-shell capture.
fn login_shell(environment: &Environment) -> bool {
    environment.source == contract::events::VariablesSource::LoginShell
}

/// The session command a `start` spawns: one `-c` per override, in
/// order. An override travels through `to_argv`, which keeps a value
/// starting with `-` attached to its flag.
#[allow(
    clippy::too_many_arguments,
    reason = "the spawn call takes each launch option once"
)]
fn start_command(
    exe: &Path,
    id: &SessionId,
    workspace: &Path,
    model: Option<&str>,
    overrides: &[&str],
    worktree: bool,
    continues: Option<&SessionId>,
    login_shell: bool,
) -> Command {
    session_command_from(
        exe,
        SessionArgv {
            id: id.clone(),
            workspace: workspace.to_path_buf(),
            model: model.map(str::to_owned),
            overrides: overrides.iter().map(|text| (*text).to_owned()).collect(),
            worktree,
            continues: continues.cloned(),
            login_shell,
            ..SessionArgv::default()
        },
    )
}

/// `exe` plus the argv of `args`, in its own process group with stdout
/// piped for the drain.
fn session_command_from(exe: &Path, args: SessionArgv) -> Command {
    let mut command = Command::new(exe);
    command.args(args.to_argv());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

/// `exe session --id <id> --workspace <workspace>`, with `--model` when
/// one is named, `--worktree` for a new worktree, `--resume` for a resume,
/// `--rewound-from` for a rewind's new session and `--continues` for a
/// session continuing another's worktree, in its own process
/// group with stdout piped for the drain.
#[allow(
    clippy::too_many_arguments,
    reason = "the spawn call takes each launch option once"
)]
fn session_command(
    exe: &Path,
    id: &SessionId,
    workspace: &Path,
    model: Option<&str>,
    worktree: bool,
    resume: bool,
    rewound_from: Option<&SessionId>,
    continues: Option<&SessionId>,
    login_shell: bool,
) -> Command {
    session_command_from(
        exe,
        SessionArgv {
            id: id.clone(),
            workspace: workspace.to_path_buf(),
            model: model.map(str::to_owned),
            resume,
            worktree,
            rewound_from: rewound_from.cloned(),
            continues: continues.cloned(),
            login_shell,
            ..SessionArgv::default()
        },
    )
}

/// Spawns `command` for session `id` and drains its stdout on a thread.
fn spawn(id: &SessionId, mut command: Command) -> std::io::Result<Box<dyn Started>> {
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
            let mut child = support::lock(&child);
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

impl Started for Spawned {
    fn exited(&self) -> Option<Failure> {
        let state = support::lock(&self.state.0);
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
        let mut state = support::lock(&self.state.0);
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
    let mut child = support::lock(child);
    if let Some(stdout) = child.stdout.take() {
        let mut read = BufReader::new(stdout);
        let mut buf = String::new();
        loop {
            buf.clear();
            match read.read_line(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Some(failure) = fiber_exited_error(&buf) {
                        support::lock(&shared.0).failure = Some(failure);
                    }
                }
            }
        }
    }
    match child.wait() {
        Ok(_) | Err(_) => {}
    }
    support::lock(&shared.0).exited = true;
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

#[cfg(test)]
#[path = "spawn_tests.rs"]
mod tests;
