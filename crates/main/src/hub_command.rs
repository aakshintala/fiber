//! `fiber hub` (`docs/invocation.md`, "Commands and flags"). `install`,
//! `uninstall`, `status`, `refresh`, `pair` and `token` run in the `cli` crate. The internal
//! `hub serve` is hidden, free to change, and named nowhere in the docs: it
//! builds the session starter and runs the hub, which reads configuration
//! once it holds the `run/` lock; a start failure prints one line on stderr.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::ErrorCode;
use contract::shapes::Failure;

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
        // `--installed` is the login service's hub; without it a client
        // started the hub.
        cli::HubCommands::Serve { installed } => serve(
            exe,
            if installed {
                hub::Mode::Installed
            } else {
                hub::Mode::OnDemand
            },
        ),
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

fn serve(exe: Result<PathBuf, String>, mode: hub::Mode) -> i32 {
    let home = match crate::startup::startup_home() {
        Ok(home) => home,
        Err(error) => return fail(error),
    };
    let exe = match exe {
        Ok(exe) => exe,
        Err(message) => return fail(failure(ErrorCode::IoFailed, message)),
    };
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(crate::clock::System);
    let configure = {
        let home = home.clone();
        move || match crate::startup::startup_workspace() {
            Ok(workspace) => configure(&home, Ok(workspace)),
            Err(failure) => Err(failure),
        }
    };
    // The hub's inherited environment, read once: the login-shell capture
    // falls back to it, with `FIBER_HOME` set to the hub's own home.
    let environments = hub::Environments::new(std::env::vars_os().collect(), &home);
    finish(hub::serve(
        &home,
        mode,
        configure,
        env!("CARGO_PKG_VERSION"),
        option_env!("FIBER_COMMIT"),
        Arc::new(hub::SpawnStarter::new(exe, Arc::clone(&environments))),
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

#[cfg(test)]
#[path = "hub_command_tests.rs"]
mod tests;
