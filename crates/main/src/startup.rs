//! Fiber home and the launch directory, read once with the startup error
//! maps: home keeps its configuration code and sentence, the workspace
//! reports `io_failed` naming the current directory. `startup` reads home
//! first, then the workspace.

use std::io;
use std::path::PathBuf;

use contract::ErrorCode;
use contract::shapes::Failure;

use crate::failed;

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
    let home = startup_home()?;
    let workspace = startup_workspace()?;
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
mod tests {
    use contract::ErrorCode;

    use super::{startup_home_with, startup_workspace_with};

    #[test]
    fn home_failure_keeps_its_code_and_sentence() {
        let failure =
            startup_home_with(|| Err(config::ConfigError::FiberHome("FIBER_HOME is empty.")))
                .unwrap_err();
        assert_eq!(failure.code, ErrorCode::Usage);
        assert_eq!(failure.message, "FIBER_HOME is empty.");
    }

    #[test]
    fn workspace_failure_names_the_current_directory() {
        let failure = startup_workspace_with(|| Err(std::io::Error::other("gone"))).unwrap_err();
        assert_eq!(failure.code, ErrorCode::IoFailed);
        assert_eq!(failure.message, "the current directory: gone");
    }
}
