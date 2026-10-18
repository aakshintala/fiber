//! `fiber extension install`, `update`, `remove`, `reinstall` and `list`
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
use extensions::{Origin, Provenance, Request, approved_extension, approved_extensions};

use crate::{failed, finish, finish_code, write_line};

mod approve;

struct ScopedInstall {
    workspace: PathBuf,
    project: config::ProjectKey,
}

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
pub fn extension_install(
    name_or_path: &str,
    project: bool,
    fiber_version: &str,
    clock: &dyn Clock,
) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let scope = if project {
                let workspace = std::env::current_dir()
                    .map_err(|e| failed(contract::ErrorCode::IoFailed, e))?;
                let (_, project) = crate::project_of(&home, &workspace)?;
                Some(ScopedInstall { workspace, project })
            } else {
                None
            };
            let request = if extensions::is_path(name_or_path) {
                Request::Path(PathBuf::from(name_or_path))
            } else {
                Request::Install(name_or_path.to_owned())
            };
            install(
                &home,
                request,
                scope.as_ref(),
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
    finish_code(ran)
}

/// `fiber extension update [<name>]`: with a name, reinstalls that
/// extension at its newest version; without one, updates every installed
/// extension, dependencies included, in listing order: each member moves to
/// its newest tag within the accepted major and records a floor, without
/// becoming a person request.
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
                    None,
                    fiber_version,
                    &Origin::github(),
                    clock,
                    io,
                ),
                None => update_all(&home, fiber_version, &Origin::github(), clock, io),
            }
        });
    finish_code(ran)
}

/// `fiber extension remove <name>`: in the launch project, never's a
/// declared repository extension for its current content, so it is not
/// offered again until its content changes; anything else removes a
/// personal install, asking first in a terminal.
pub fn extension_remove(name: &str, clock: &dyn Clock) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace =
                std::env::current_dir().map_err(|e| failed(contract::ErrorCode::IoFailed, e))?;
            remove_in(
                &home,
                &workspace,
                name,
                clock,
                Io {
                    terminal,
                    input: &mut stdin.lock(),
                    err: &mut io::stderr(),
                },
            )
        });
    finish_code(ran)
}

/// `fiber extension reinstall <name>`: removes the extension and installs
/// it again from its recorded source and commit, or from `<name>` as given
/// when it is damaged, asking first in a terminal.
pub fn extension_reinstall(name: &str, fiber_version: &str, clock: &dyn Clock) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            install(
                &home,
                Request::Reinstall(name.to_owned()),
                None,
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
    finish_code(ran)
}

/// `fiber extension list`: one line per installed extension: name,
/// version and commit; then one line per approved repository extension
/// of the launch project: name, version, `repository`, the workspace,
/// the declared path and the approved content hash.
pub fn extension_list(clock: &dyn Clock) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace =
                std::env::current_dir().map_err(|e| failed(contract::ErrorCode::IoFailed, e))?;
            list_in(
                &home,
                &workspace,
                clock,
                &mut io::stdout().lock(),
                &mut io::stderr(),
            )
        });
    finish(ran)
}

fn install(
    home: &Path,
    request: Request,
    scope: Option<&ScopedInstall>,
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
    report_install(commit_plan(home, scope, plan, terminal, input, err), err)
}

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
    for ext in &listing.installed {
        let plan = plan_request(
            home,
            Request::UpdateAll(ext.name.clone()),
            fiber_version,
            origin,
            clock,
        )?;
        let code = report_install(commit_plan(home, None, plan, terminal, input, err), err)?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

fn remove(home: &Path, typed: &str, clock: &dyn Clock, io: Io<'_>) -> Result<i32, Failure> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let removal = extensions::removal(home, typed, clock).map_err(|e| failed(e.code(), e))?;
    let approved = approve::remove_approved(&removal.names, &removal.data, terminal, input, err)?;
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

/// `fiber extension list` in `workspace`: the personal lines, then one
/// row per declared extension this project approved, at its current
/// content: name, version, `repository`, the workspace, the declared
/// path and the content hash. A declaration that does not read names
/// itself once and leaves the personal lines alone.
fn list_in(
    home: &Path,
    workspace: &Path,
    clock: &dyn Clock,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let (_, project) = crate::project_of(home, workspace)?;
    let approved = match approved_extensions(home, &project, workspace, clock) {
        Ok(approved) => approved,
        Err(e) => {
            writeln!(err, "fiber: {e}").unwrap_or(());
            Vec::new()
        }
    };
    list(home, clock, out)?;
    for entry in &approved {
        // A closed stdout leaves nobody to tell.
        write_line(
            out,
            &format!(
                "{} {} repository {} {} {}",
                entry.name,
                entry.version,
                workspace.display(),
                entry.path,
                entry.hash
            ),
        )
        .unwrap_or(());
    }
    Ok(())
}

/// `fiber extension remove <name>` in `workspace`: a declared extension
/// whose current content this project approved is recorded never for
/// that content, so it is not offered again until its content changes;
/// the pinned copy, a personal install of the same name and its data
/// stay. Anything else falls through to the personal removal, asking
/// first in a terminal.
fn remove_in(
    home: &Path,
    workspace: &Path,
    typed: &str,
    clock: &dyn Clock,
    io: Io<'_>,
) -> Result<i32, Failure> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let (_, project) = crate::project_of(home, workspace)?;
    let found = match approved_extension(home, &project, workspace, typed, clock) {
        Ok(found) => found,
        Err(e) => {
            writeln!(err, "fiber: {e}").unwrap_or(());
            None
        }
    };
    let Some((item, digest)) = found else {
        return remove(
            home,
            typed,
            clock,
            Io {
                terminal,
                input,
                err,
            },
        );
    };
    if !approve::remove_approved(std::slice::from_ref(&item.name), &[], terminal, input, err)? {
        writeln!(err, "fiber: nothing was removed.").unwrap_or(());
        return Ok(1);
    }
    extensions::Store::never_in(home, &project, &item, &digest).map_err(|e| failed(e.code(), e))?;
    writeln!(
        err,
        "fiber: removed {} from this project; it is not offered again until its content changes",
        item.name
    )
    .unwrap_or(());
    Ok(0)
}

fn list(home: &Path, clock: &dyn Clock, out: &mut dyn Write) -> Result<(), Failure> {
    let listing = extensions::list(home, clock).map_err(|e| failed(e.code(), e))?;
    for hit in &listing.damaged {
        write_line(out, &format!("{hit}")).unwrap_or(());
    }
    for i in &listing.installed {
        let commit = match &i.provenance {
            Provenance::Git { commit } => commit.as_str(),
            Provenance::Path(_) => "local",
        };
        // A closed stdout leaves nobody to tell.
        write_line(out, &format!("{} {} {commit}", i.name, i.version)).unwrap_or(());
    }
    Ok(())
}

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

fn plan_request<'c>(
    home: &Path,
    request: Request,
    fiber_version: &str,
    origin: &Origin,
    clock: &'c dyn Clock,
) -> Result<extensions::Plan<'c>, Failure> {
    extensions::plan(home, &request, fiber_version, origin, clock).map_err(|e| failed(e.code(), e))
}

fn commit_plan(
    home: &Path,
    scope: Option<&ScopedInstall>,
    plan: extensions::Plan<'_>,
    terminal: bool,
    input: &mut dyn BufRead,
    err: &mut dyn Write,
) -> Result<Option<Vec<String>>, Failure> {
    let summaries: Vec<approve::InstallSummary> = plan
        .items()
        .map(|item| approve::InstallSummary {
            name: item.name.clone(),
            source: item.source(),
            version: item.version.clone(),
            changes: item.changes.clone(),
            replaces: item.manifest.replaces.clone(),
            providers: item
                .manifest
                .providers
                .iter()
                .map(|(name, urls)| {
                    let mut urls = urls.clone();
                    urls.sort();
                    urls.dedup();
                    (name.clone(), urls)
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
    if !approve::install_approved(&summaries, terminal, input, err)? {
        return Ok(None);
    }
    if let Some(scope) = scope {
        for name in plan.items().map(|item| item.name.as_str()) {
            config::set(
                home,
                &scope.workspace,
                &scope.project,
                config::Layer::Project,
                &format!("extensions.\"{}\".enabled", config::short_name(name)),
                serde_json::Value::Bool(true),
            )
            .map_err(|e| failed(e.code(), e))?;
        }
        for name in plan.fresh_names() {
            config::set(
                home,
                &scope.workspace,
                &scope.project,
                config::Layer::Global,
                &format!("extensions.\"{}\".enabled", config::short_name(name)),
                serde_json::Value::Bool(false),
            )
            .map_err(|e| failed(e.code(), e))?;
        }
    }
    plan.commit().map(Some).map_err(|e| failed(e.code(), e))
}

#[cfg(test)]
#[path = "extension_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "extension_repo_tests.rs"]
mod repo_tests;
