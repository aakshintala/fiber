//! The commands that run no session: `fiber config`, `fiber login`,
//! `fiber logout`, `fiber approve`, `fiber sessions`, `fiber sessions export`,
//! `fiber sessions delete`, `fiber sessions prune`, `fiber sessions search`, `fiber models`,
//! `fiber extension install`, `fiber extension update`,
//! `fiber extension install`, `fiber extension update`,
//! `fiber extension remove`, `fiber extension list`, `fiber extension test`, `fiber mcp add`,
//! `fiber mcp remove`, `fiber mcp list`, `fiber mcp login`, `fiber mcp logout`, `fiber hub install`,
//! `fiber hub uninstall`, `fiber hub status`, `fiber hub refresh`, and the hub restart
//! `fiber update` calls, and the release install step `install.sh` runs
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
mod extension;
mod extension_test;
mod hub_pair;
mod hub_refresh;
mod hub_service;
mod hub_status;
mod hub_token;
mod hub_unit;
mod login;
mod mcp;
mod mcp_login;
mod model_lists;
mod models;
mod prune;
mod release;
mod sessions;
mod sessions_list;
mod sessions_search;
mod table;

#[cfg(test)]
mod test_support;

pub use approve::approve;
pub use config::{config_get, config_set, config_set_text};
pub use extension::{
    extension_install, extension_list, extension_reinstall, extension_remove, extension_update,
};
pub use extension_test::extension_test;
pub use hub_pair::{Address, hub_pair};
pub use hub_refresh::hub_refresh;
pub use hub_service::{hub_install, hub_restart, hub_uninstall};
pub use hub_status::hub_status;
pub use hub_token::{hub_token_list, hub_token_revoke};
pub use login::{
    LoginCancel, LoginName, LoginStored, LogoutTarget, browser_login, login_store, login_targets,
    providers_in, run_login, run_logout,
};
pub use mcp::{McpDeclaration, mcp_add, mcp_list, mcp_remove};
pub use mcp_login::{mcp_login, mcp_logout};
pub use model_lists::{ModelLists, ModelListsError, model_lists, roles_of};
pub use models::{models, refresh_model_lists};
pub use prune::{PruneArgs, prune};
pub use release::release_install;
pub use sessions::{delete, export};
pub use sessions_list::list as sessions_list;
pub use sessions_search::search as sessions_search;

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
