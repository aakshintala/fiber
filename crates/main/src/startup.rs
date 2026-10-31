//! Fiber home and the launch directory, read once with the startup error
//! maps: home keeps its configuration code and sentence, the workspace
//! reports `io_failed` naming the current directory. `startup` reads home
//! first, then the workspace.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use contract::ErrorCode;
use contract::shapes::Failure;

use crate::failed;

/// Process-wide inputs shared by session startup and hub clients.
pub(crate) struct Process {
    pub(crate) clock: Arc<dyn contract::clock::Clock>,
    pub(crate) exe: Result<PathBuf, Failure>,
}

/// Fiber home with today's home map.
pub(crate) fn startup_home() -> Result<PathBuf, Failure> {
    startup_home_with(config::fiber_home_from_env)
}

/// The launch directory with today's current-directory map.
pub(crate) fn startup_workspace() -> Result<PathBuf, Failure> {
    startup_workspace_with(std::env::current_dir)
}

/// Home first, then the workspace, for paths that read both eagerly.
pub(crate) fn startup() -> Result<(PathBuf, PathBuf), Failure> {
    startup_with(config::fiber_home_from_env, std::env::current_dir)
}

/// `startup` over injected reads, so the precedence test pins the home
/// error without touching the environment or the process directory.
fn startup_with(
    home: impl FnOnce() -> Result<PathBuf, config::ConfigError>,
    workspace: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<(PathBuf, PathBuf), Failure> {
    let home = startup_home_with(home)?;
    let workspace = startup_workspace_with(workspace)?;
    Ok((home, workspace))
}

/// `startup_home` over an injected home read, so tests pin the error map
/// without touching the environment.
fn startup_home_with(
    home: impl FnOnce() -> Result<PathBuf, config::ConfigError>,
) -> Result<PathBuf, Failure> {
    home().map_err(|e| failed(e.code(), e))
}

/// `startup_workspace` over an injected directory read, so tests pin the
/// error map without touching the process directory.
fn startup_workspace_with(
    workspace: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<PathBuf, Failure> {
    workspace().map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
