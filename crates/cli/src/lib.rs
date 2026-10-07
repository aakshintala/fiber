//! The commands that run no session: `fiber config`, `fiber login`,
//! `fiber logout`, `fiber approve`, `fiber sessions export` and `fiber models`
//! (`docs/architecture.md`, "The modules"). `main` parses argv and
//! dispatches here; this crate takes plain values.

use std::fmt::Display;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::shapes::Failure;
use doors::failure;

mod approve;
mod config;
mod login;
mod models;
mod sessions;

pub use approve::approve;
pub use config::{config_get, config_set};
pub use login::{LogoutTarget, run_login, run_logout};
pub use models::{models, refresh_model_lists};
pub use sessions::export;

/// What `fiber logout` says when it is given no provider.
pub const LOGOUT_SHAPE: &str =
    "`fiber logout` takes the provider to log out of. Run `fiber --help` for usage.";

/// A workspace's project: its `sessions` directory and its key
/// (`docs/state.md`, "Projects"). The one place the key is derived.
pub fn project_of(
    home: &Path,
    workspace: &Path,
) -> Result<(PathBuf, ::config::ProjectKey), Failure> {
    let sessions = log::sessions_dir(home, &doors::project(workspace));
    // `projects/<key>/sessions`: the project's key names its parent.
    let key = sessions
        .parent()
        .and_then(Path::file_name)
        .map(|key| key.to_string_lossy().into_owned())
        .unwrap_or_default();
    let project = ::config::ProjectKey::new(key).map_err(|e| failed(e.code(), e))?;
    Ok((sessions, project))
}

fn usage(message: impl Into<String>) -> Failure {
    failure(ErrorCode::Usage, message)
}

fn failed(code: ErrorCode, e: impl Display) -> Failure {
    failure(code, e.to_string())
}

/// Prints a failure the way every command does, and gives its exit code.
fn fail(e: Failure) -> i32 {
    // A closed stderr leaves nobody to tell.
    writeln!(io::stderr(), "fiber: {}", e.message).unwrap_or(());
    doors::exit_code(&e)
}
