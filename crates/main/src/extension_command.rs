//! The `fiber extension` subcommands (`docs/extensions.md`, "Installing"):
//! install, update, remove and list.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use contract::shapes::Failure;
use extensions::{Origin, Provenance, Request};

use crate::cli;

/// Runs the `fiber extension` subcommands.
pub(crate) fn extension(cmd: cli::ExtensionCommands, clock: &dyn contract::clock::Clock) -> i32 {
    match cmd {
        cli::ExtensionCommands::Install { name_or_path } => {
            let request = if extensions::is_path(&name_or_path) {
                Request::Path(PathBuf::from(name_or_path))
            } else {
                Request::Install(name_or_path)
            };
            install(request, clock)
        }
        cli::ExtensionCommands::Update { name: Some(name) } => {
            install(Request::Update(name), clock)
        }
        cli::ExtensionCommands::Update { name: None } => update_all(clock),
        cli::ExtensionCommands::Remove { name } => remove(&name, clock),
        cli::ExtensionCommands::List => list(clock),
    }
}

/// `fiber extension update` with no name: each extension a person asked for.
fn update_all(clock: &dyn contract::clock::Clock) -> i32 {
    let home = match config::fiber_home_from_env() {
        Ok(home) => home,
        Err(e) => return crate::fail(crate::failed(e.code(), e)),
    };
    let listing = match extensions::list(&home, clock) {
        Ok(list) => list,
        Err(e) => return crate::fail(crate::failed(e.code(), e)),
    };
    // Each damaged directory is named once here; the per-extension
    // installs it runs stay silent about them.
    for hit in &listing.damaged {
        eprintln!("fiber: {}", hit.skipped());
    }
    for ext in listing.installed.iter().filter(|i| i.requested) {
        let plan = match plan_request(Request::Update(ext.name.clone()), clock) {
            Ok(plan) => plan,
            Err(e) => return crate::fail(e),
        };
        let code = report_install(commit_plan(plan));
        if code != 0 {
            return code;
        }
    }
    0
}

/// `fiber extension install <name or path>` and `fiber extension update <name>`: fetches and
/// checks the extension and its dependencies, shows what they register and
/// asks when stdin is a terminal (`docs/extensions.md`, "Installing"), and
/// prints each name installed.
fn install(request: Request, clock: &dyn contract::clock::Clock) -> i32 {
    let plan = match plan_request(request, clock) {
        Ok(plan) => plan,
        Err(e) => return crate::fail(e),
    };
    // The plan's damaged directories are named before approval, so a
    // declined or failed install still names them.
    for hit in plan.damaged() {
        eprintln!("fiber: {}", hit.skipped());
    }
    report_install(commit_plan(plan))
}

/// Prints what an approved install did: each name installed, or that
/// nothing was installed.
fn report_install(result: Result<Option<Vec<String>>, Failure>) -> i32 {
    match result {
        Ok(Some(names)) => {
            for name in names {
                eprintln!("fiber: installed {name}");
            }
            0
        }
        Ok(None) => {
            eprintln!("fiber: nothing was installed.");
            1
        }
        Err(e) => crate::fail(e),
    }
}

/// Fetches and checks what `request` and its dependencies need.
fn plan_request(
    request: Request,
    clock: &dyn contract::clock::Clock,
) -> Result<extensions::Plan, Failure> {
    let home = config::fiber_home_from_env().map_err(|e| crate::failed(e.code(), e))?;
    extensions::plan(
        &home,
        &request,
        env!("CARGO_PKG_VERSION"),
        &Origin::github(),
        clock,
    )
    .map_err(|e| crate::failed(e.code(), e))
}

/// Shows what `plan` registers, asks when stdin is a terminal, and
/// installs once approved; `None` when the person declined.
fn commit_plan(plan: extensions::Plan) -> Result<Option<Vec<String>>, Failure> {
    let summaries: Vec<doors::InstallSummary> = plan
        .items()
        .map(|item| doors::InstallSummary {
            name: item.name.clone(),
            source: item.source(),
            version: item.version.clone(),
            changes: item.changes.clone(),
            providers: item
                .providers
                .iter()
                .map(|p| {
                    let mut urls: Vec<String> =
                        p.models.iter().map(|m| m.base_url.clone()).collect();
                    urls.sort();
                    urls.dedup();
                    (p.name.clone(), urls)
                })
                .collect(),
            process: item.manifest.process.as_ref().map(|p| {
                std::iter::once(p.program.as_str())
                    .chain(p.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
            install_step: item.manifest.install.as_ref().map(|step| step.join(" ")),
            carries: item.carries(),
            staged: item.staged().to_path_buf(),
        })
        .collect();
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    if !doors::install_approved(&summaries, terminal, &mut stdin.lock(), &mut io::stderr())? {
        return Ok(None);
    }
    plan.commit()
        .map(Some)
        .map_err(|e| crate::failed(e.code(), e))
}

/// `fiber extension remove <name>`: removes an extension, the dependencies nothing
/// else uses, and their data and settings, asking first in a terminal.
fn remove(typed: &str, clock: &dyn contract::clock::Clock) -> i32 {
    let removed = config::fiber_home_from_env()
        .map_err(|e| crate::failed(e.code(), e))
        .and_then(|home| {
            let removal =
                extensions::removal(&home, typed, clock).map_err(|e| crate::failed(e.code(), e))?;
            let stdin = io::stdin();
            let terminal = stdin.is_terminal();
            let approved = doors::remove_approved(
                &removal.names,
                &removal.data,
                terminal,
                &mut stdin.lock(),
                &mut io::stderr(),
            )?;
            if !approved {
                return Ok(None);
            }
            let names = removal.names.clone();
            removal.commit().map_err(|e| crate::failed(e.code(), e))?;
            Ok(Some(names))
        });
    match removed {
        Ok(Some(names)) => {
            for name in names {
                eprintln!("fiber: removed {name}");
            }
            0
        }
        Ok(None) => {
            eprintln!("fiber: nothing was removed.");
            1
        }
        Err(e) => crate::fail(e),
    }
}

/// `fiber extension list`: one line per installed extension: name, version and commit.
fn list(clock: &dyn contract::clock::Clock) -> i32 {
    let listed = config::fiber_home_from_env()
        .map_err(|e| crate::failed(e.code(), e))
        .and_then(|home| extensions::list(&home, clock).map_err(|e| crate::failed(e.code(), e)));
    match listed {
        Ok(listing) => {
            let mut out = io::stdout().lock();
            // Each damaged directory first, one line each, then the
            // healthy rows.
            for hit in &listing.damaged {
                writeln!(out, "{hit}").unwrap_or(());
            }
            for i in listing.installed {
                let commit = match &i.provenance {
                    Provenance::Git { commit } => commit.as_str(),
                    Provenance::Path(_) => "local",
                };
                // A closed stdout leaves nobody to tell.
                writeln!(out, "{} {} {commit}", i.name, i.version).unwrap_or(());
            }
            0
        }
        Err(e) => crate::fail(e),
    }
}
