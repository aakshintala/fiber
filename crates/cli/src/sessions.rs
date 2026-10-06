//! `fiber sessions export` (`docs/invocation.md`, "Deleting and pruning"):
//! copies a session's log lines and its `artifacts/` into a new directory.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::shapes::Failure;

use crate::{fail, failed, project_of};

/// `fiber sessions export <id> [<path>]` in the current directory: resolves
/// the session, copies it, and prints the export directory's path.
pub fn export(selector: &str, path: Option<&Path>) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            run(&home, &workspace, selector, path, &mut io::stdout())
        });
    match ran {
        Ok(_) => 0,
        Err(e) => fail(e),
    }
}

/// Resolves `selector` in the current directory's project and exports the
/// session into `path`, defaulting to the full session id under the current
/// directory. A relative `path` is taken from the current directory. Gives
/// the export directory's absolute path.
fn run(
    home: &Path,
    workspace: &Path,
    selector: &str,
    path: Option<&Path>,
    out: &mut dyn Write,
) -> Result<PathBuf, Failure> {
    let (sessions, _) = project_of(home, workspace)?;
    let id = log::resolve(&sessions, selector).map_err(|e| failed(e.code(), e))?;
    let target = match path {
        Some(path) => workspace.join(path),
        None => workspace.join(&id.0),
    };
    log::export(&sessions.join(&id.0), &target).map_err(|e| failed(e.code(), e))?;
    writeln!(out, "{}", target.display())
        .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
    Ok(target)
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
