//! `fiber extension install`, `update`, `remove` and `list`
//! (`docs/extensions.md`, "Installing"): install fetches and checks an
//! extension and its dependencies, shows what they register and asks
//! before going ahead in a terminal; update moves one or every installed
//! extension to its newest tag; remove deletes an extension and what
//! nothing else uses; list names what is installed. `main` parses argv
//! and dispatches here; this crate takes plain values.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use contract::clock::Clock;
use contract::shapes::Failure;
use extensions::{Origin, Provenance, Request};

use crate::{fail, failed};

/// What a command asks on and reports to: whether stdin is a terminal,
/// stdin, and stderr.
struct Io<'a> {
    terminal: bool,
    input: &'a mut dyn BufRead,
    err: &'a mut dyn Write,
}

/// `fiber extension install <name or path>`: fetches and checks the
/// extension and its dependencies, shows what they register and asks
/// when stdin is a terminal, and prints each name installed.
pub fn extension_install(name_or_path: &str, fiber_version: &str, clock: &dyn Clock) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let request = if extensions::is_path(name_or_path) {
                Request::Path(PathBuf::from(name_or_path))
            } else {
                Request::Install(name_or_path.to_owned())
            };
            install(
                &home,
                request,
                fiber_version,
                &Origin::github(),
                clock,
                Io {
                    terminal,
                    input: &mut stdin.lock(),
                    err: &mut io::stderr(),
                },
            )
        });
    match ran {
        Ok(code) => code,
        Err(e) => fail(e),
    }
}

/// `fiber extension update [<name>]`: with a name, reinstalls that
/// extension at its newest version; without one, updates every
/// installed extension a person asked for, in listing order.
pub fn extension_update(name: Option<&str>, fiber_version: &str, clock: &dyn Clock) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let io = Io {
                terminal,
                input: &mut stdin.lock(),
                err: &mut io::stderr(),
            };
            match name {
                Some(name) => install(
                    &home,
                    Request::Update(name.to_owned()),
                    fiber_version,
                    &Origin::github(),
                    clock,
                    io,
                ),
                None => update_all(&home, fiber_version, &Origin::github(), clock, io),
            }
        });
    match ran {
        Ok(code) => code,
        Err(e) => fail(e),
    }
}

/// `fiber extension remove <name>`: removes an extension, the
/// dependencies nothing else uses, and their data and settings, asking
/// first in a terminal.
pub fn extension_remove(name: &str, clock: &dyn Clock) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            remove(
                &home,
                name,
                clock,
                Io {
                    terminal,
                    input: &mut stdin.lock(),
                    err: &mut io::stderr(),
                },
            )
        });
    match ran {
        Ok(code) => code,
        Err(e) => fail(e),
    }
}

/// `fiber extension list`: one line per installed extension: name,
/// version and commit.
pub fn extension_list(clock: &dyn Clock) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| list(&home, clock, &mut io::stdout().lock()));
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// Installs `request`: fetches and checks what it and its dependencies
/// need, shows what they register and asks when the terminal says so,
/// and prints each name installed.
fn install(
    home: &Path,
    request: Request,
    fiber_version: &str,
    origin: &Origin,
    clock: &dyn Clock,
    io: Io<'_>,
) -> Result<i32, Failure> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let plan = plan_request(home, request, fiber_version, origin, clock)?;
    // The plan's damaged directories are named before approval, so a
    // declined or failed install still names them.
    for hit in plan.damaged() {
        writeln!(err, "fiber: {}", hit.skipped()).unwrap_or(());
    }
    report_install(commit_plan(plan, terminal, input, err), err)
}

/// Updates every installed extension a person asked for, in listing
/// order, stopping at the first declined install or the first failure.
fn update_all(
    home: &Path,
    fiber_version: &str,
    origin: &Origin,
    clock: &dyn Clock,
    io: Io<'_>,
) -> Result<i32, Failure> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let listing = extensions::list(home, clock).map_err(|e| failed(e.code(), e))?;
    // Each damaged directory is named once here; the per-extension
    // installs below stay silent about them.
    for hit in &listing.damaged {
        writeln!(err, "fiber: {}", hit.skipped()).unwrap_or(());
    }
    for ext in listing.installed.iter().filter(|i| i.requested) {
        let plan = plan_request(
            home,
            Request::Update(ext.name.clone()),
            fiber_version,
            origin,
            clock,
        )?;
        let code = report_install(commit_plan(plan, terminal, input, err), err)?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

/// Removes the extension `typed` names, asking first in a terminal.
fn remove(home: &Path, typed: &str, clock: &dyn Clock, io: Io<'_>) -> Result<i32, Failure> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let removal = extensions::removal(home, typed, clock).map_err(|e| failed(e.code(), e))?;
    let approved = doors::remove_approved(&removal.names, &removal.data, terminal, input, err)?;
    if !approved {
        writeln!(err, "fiber: nothing was removed.").unwrap_or(());
        return Ok(1);
    }
    let names = removal.names.clone();
    removal.commit().map_err(|e| failed(e.code(), e))?;
    for name in names {
        writeln!(err, "fiber: removed {name}").unwrap_or(());
    }
    Ok(0)
}

/// Lists what `home` holds: each damaged directory first, then one line
/// per installed extension.
fn list(home: &Path, clock: &dyn Clock, out: &mut dyn Write) -> Result<(), Failure> {
    let listing = extensions::list(home, clock).map_err(|e| failed(e.code(), e))?;
    // Each damaged directory first, one line each, then the healthy
    // rows.
    for hit in &listing.damaged {
        writeln!(out, "{hit}").unwrap_or(());
    }
    for i in &listing.installed {
        let commit = match &i.provenance {
            Provenance::Git { commit } => commit.as_str(),
            Provenance::Path(_) => "local",
        };
        // A closed stdout leaves nobody to tell.
        writeln!(out, "{} {} {commit}", i.name, i.version).unwrap_or(());
    }
    Ok(())
}

/// Prints what an approved install did: each name installed, or that
/// nothing was installed.
fn report_install(
    result: Result<Option<Vec<String>>, Failure>,
    err: &mut dyn Write,
) -> Result<i32, Failure> {
    match result {
        Ok(Some(names)) => {
            for name in names {
                writeln!(err, "fiber: installed {name}").unwrap_or(());
            }
            Ok(0)
        }
        Ok(None) => {
            writeln!(err, "fiber: nothing was installed.").unwrap_or(());
            Ok(1)
        }
        Err(e) => Err(e),
    }
}

/// Fetches and checks what `request` and its dependencies need.
fn plan_request(
    home: &Path,
    request: Request,
    fiber_version: &str,
    origin: &Origin,
    clock: &dyn Clock,
) -> Result<extensions::Plan, Failure> {
    extensions::plan(home, &request, fiber_version, origin, clock).map_err(|e| failed(e.code(), e))
}

/// Shows what `plan` registers, asks when the terminal says so, and
/// installs once approved; `None` when the person declined.
fn commit_plan(
    plan: extensions::Plan,
    terminal: bool,
    input: &mut dyn BufRead,
    err: &mut dyn Write,
) -> Result<Option<Vec<String>>, Failure> {
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
    if !doors::install_approved(&summaries, terminal, input, err)? {
        return Ok(None);
    }
    plan.commit().map(Some).map_err(|e| failed(e.code(), e))
}

#[cfg(test)]
#[path = "extension_tests.rs"]
mod tests;
