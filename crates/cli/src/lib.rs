//! The commands that run no session: `fiber config`, `fiber login`,
//! `fiber logout`, `fiber approve`, `fiber sessions`, `fiber sessions export`,
//! `fiber sessions delete`, `fiber sessions prune`, `fiber sessions search`, `fiber models`,
//! `fiber extension install`, `fiber extension update`,
//! `fiber extension install`, `fiber extension update`,
//! `fiber extension remove`, `fiber extension list`, `fiber extension test`, `fiber mcp add`,
//! `fiber mcp remove`, `fiber mcp list`, `fiber mcp login`, `fiber mcp logout`, `fiber hub install`,
//! `fiber hub uninstall`, `fiber hub status`, `fiber hub refresh`, `fiber hub add`,
//! `fiber hub remove`, `fiber doctor`, and the hub restart
//! `fiber update` calls, `fiber setup`, and the release install step `install.sh` runs
//! (`docs/architecture.md`, "The modules"). `main` parses argv and
//! dispatches here; this crate takes plain values.

use std::fmt::Display;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::shapes::Failure;
use doors::failure;

mod error;

pub use error::{CliError, Refusal};

#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod approve;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod config;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod doctor;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod extension;
mod extension_test;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod feedback;
mod hub_env;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod hub_list;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod hub_pair;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod hub_refresh;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod hub_service;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod hub_status;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod hub_token;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod hub_unit;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod import;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod login;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod mcp;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod mcp_login;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod model_lists;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod models;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod prune;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod release;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod sessions;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod sessions_list;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod sessions_search;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod setup;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod stop;
mod table;
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
mod update;

#[cfg(test)]
mod test_support;

pub use approve::approve;
pub use config::{config_get, config_set, config_set_text};
pub use doctor::{capture, doctor, with_fix};
pub use extension::{
    FirstPartyInstall, Io, LoginNeed, extension_install, extension_list, extension_reinstall,
    extension_remove, extension_update, install_first_party,
};
pub use extension_test::extension_test;
pub use feedback::diagnostics;
pub use feedback::{FeedbackError, ReportContext, build_report, select_session, write};
pub use hub_env::{declared_names, hub_env, lookup, placeholder_lookup};
pub use hub_list::{hub_add, hub_remove};
pub use hub_pair::{Address, hub_pair};
pub use hub_refresh::hub_refresh;
pub use hub_service::{hub_install, hub_restart, hub_uninstall};
pub use hub_status::hub_status;
pub use hub_token::{hub_token_list, hub_token_revoke};
pub use import::import;
pub use login::{
    LoginCancel, LoginName, LoginStored, LogoutTarget, ReviewerModels, browser_login, login_store,
    login_targets, providers_in, reviewer_models, run_login, run_logout, save_host, set_reviewer,
};
pub use mcp::{DeclaredServer, McpDeclaration, declared_servers, mcp_add, mcp_list, mcp_remove};
pub use mcp_login::{login_target, mcp_login, mcp_logout};
pub use model_lists::{ModelLists, ModelListsError, model_lists, roles_of};
pub use models::{models, refresh_model_lists};
pub use prune::{PruneArgs, prune};
pub use release::release_install;
pub use sessions::{delete, export};
pub use sessions_list::list as sessions_list;
pub use sessions_list::resolve_listed;
pub use sessions_search::search as sessions_search;
pub use setup::{Completions, Hub, SetupFlags, setup};
pub use stop::{stop_local, stop_remote};
pub use update::{Channel, ChannelError, ReleaseBases, resolve_channel, update};

/// What `fiber logout` says when it is given no provider.
pub const LOGOUT_SHAPE: &str =
    "`fiber logout` takes the provider to log out of. Run `fiber --help` for usage.";

/// A workspace's project: its `sessions` directory and its key
/// (`docs/state.md`, "Projects"). The key comes from `log::project_key`,
/// the one place it is derived.
#[allow(
    clippy::result_large_err,
    reason = "every command returns the finest-grained error by value; the error path is cold"
)]
pub fn project_of(
    home: &Path,
    workspace: &Path,
) -> Result<(PathBuf, ::config::ProjectKey), CliError> {
    let project = doors::project(workspace);
    let sessions = log::sessions_dir(home, &project);
    let key = ::config::ProjectKey::new(log::project_key(&project))?;
    Ok((sessions, key))
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

/// Maps a command result to its exit code through the one error branch.
fn finish<E: Into<Failure>>(result: Result<(), E>) -> i32 {
    finish_code(result.map(|()| 0))
}

/// Maps an integer command result to its exit code: the value on success,
/// the failure's code once printed on error.
fn finish_code<E: Into<Failure>>(result: Result<i32, E>) -> i32 {
    match result {
        Ok(code) => code,
        Err(e) => fail(e.into()),
    }
}

/// Writes one line to `out`: the line plus one line feed. A write failure
/// is the shared standard-output failure, for the caller to propagate or
/// ignore as it already did.
fn write_line(out: &mut dyn Write, line: &str) -> Result<(), Failure> {
    writeln!(out, "{line}")
        .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))
}

/// Runs `run` with Fiber home and the current directory, mapping any
/// failure to its exit code.
fn with_dirs<E: Into<Failure>>(run: impl FnOnce(&Path, &Path) -> Result<(), E>) -> i32 {
    with_dirs_code(|home, workspace| run(home, workspace).map(|()| 0))
}

/// Runs `run` with Fiber home and the current directory, keeping its
/// integer success code and mapping any failure to its exit code. Home
/// is read before the current directory, as each wrapper did.
fn with_dirs_code<E: Into<Failure>>(run: impl FnOnce(&Path, &Path) -> Result<i32, E>) -> i32 {
    let ran = ::config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            run(&home, &workspace).map_err(Into::into)
        });
    finish_code(ran)
}
