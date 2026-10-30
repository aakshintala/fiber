//! `fiber extension install`, `update`, `remove`, `reinstall` and `list`
//! (`docs/extensions.md`, "Installing"): install fetches and checks an
//! extension and its dependencies, shows what they register and asks
//! before going ahead in a terminal; update moves one or every installed
//! extension to its newest tag; remove deletes an extension and what
//! nothing else uses; list names what is installed. `main` parses argv
//! and dispatches here; this crate takes plain values.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::clock::Clock;
use extensions::{Origin, Provenance, Request, approved_extension, approved_extensions};

use crate::{CliError, finish, finish_code, write_line};

pub(crate) mod approve;

/// The extensions installed and the outcome text for the model picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstPartyInstall {
    /// Each extension put in place.
    pub installed: Vec<String>,
    /// The install summary, one line per element.
    pub summary: Vec<String>,
    /// Providers whose selected credential is missing or unreadable.
    pub login: Vec<LoginNeed>,
}

/// A provider needing a key, or whose key could not be checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginNeed {
    /// The provider to pass to `fiber login`.
    pub provider: String,
    /// A credential-read failure; absent when the key is missing.
    pub error: Option<String>,
}

/// Installs the first-party package serving a provider, without asking.
/// The plan's binary downloads record on `debug`; `None` stays silent.
pub fn install_first_party(
    home: &Path,
    config: &config::Config,
    provider: &str,
    fiber_version: &str,
    origin: &Origin,
    clock: &dyn Clock,
    debug: Option<Arc<dyn contract::diag::DebugLog>>,
) -> Result<FirstPartyInstall, CliError> {
    let package =
        extensions::serving_package(provider).ok_or_else(|| CliError::NoFirstPartyPackage {
            provider: provider.to_owned(),
        })?;
    let plan = plan_request(
        home,
        Request::Install(package.to_owned()),
        fiber_version,
        origin,
        clock,
    )
    .map(|plan| plan.with_diagnostics(debug))?;
    let summary = plan
        .damaged()
        .iter()
        .map(extensions::Damaged::skipped)
        .chain(
            approve::summary_text(&summaries(&plan))
                .lines()
                .map(str::to_owned),
        )
        .collect();
    let (providers, _) = extensions::Providers::load_planned(home, &plan)?;
    let names: std::collections::BTreeSet<_> = plan
        .items()
        .flat_map(|item| item.providers.iter().map(|data| data.name.as_str()))
        .collect();
    let credentials = config.credentials();
    let mut login = Vec::new();
    for name in names {
        let Some(data) = providers.get(name) else {
            continue;
        };
        let label = credentials.credential_label(data);
        let need = match credentials.key_presence(data, &label, &|var| std::env::var(var).ok()) {
            Ok(config::Presence::Present | config::Presence::Unchecked) => None,
            Ok(config::Presence::Absent | config::Presence::Empty) => Some(LoginNeed {
                provider: name.to_owned(),
                error: None,
            }),
            Err(error) => Some(LoginNeed {
                provider: name.to_owned(),
                error: Some(error.to_string()),
            }),
        };
        if let Some(need) = need {
            login.push(need);
        }
    }
    let installed = plan.commit()?;
    Ok(FirstPartyInstall {
        installed,
        summary,
        login,
    })
}

struct ScopedInstall {
    workspace: PathBuf,
    project: config::ProjectKey,
}

/// What a command asks on and reports to: whether stdin is a terminal,
/// stdin, and stderr. Shared with `update`, whose terminal question asks
/// through the same seam: `terminal` from `stdin.is_terminal()` at the
/// call site, `input` its lock, `err` standard error. Stays `pub` for the
/// `fiber update` dispatch in `main`, which builds it at the call site.
pub struct Io<'a> {
    /// Whether stdin is a terminal: without one there is no question.
    pub terminal: bool,
    /// Standard input, to read the answer from.
    pub input: &'a mut dyn BufRead,
    /// Standard error, to print the question on.
    pub err: &'a mut dyn Write,
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
        .map_err(CliError::from)
        .and_then(|home| {
            let scope = if project {
                let workspace = std::env::current_dir().map_err(CliError::ExtensionDirectory)?;
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
        .map_err(CliError::from)
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
        .map_err(CliError::from)
        .and_then(|home| {
            let workspace = std::env::current_dir().map_err(CliError::ExtensionDirectory)?;
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
        .map_err(CliError::from)
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
/// of the launch project: name, version, `repository`, the project,
/// the declared path and the approved content hash.
pub fn extension_list(clock: &dyn Clock) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(CliError::from)
        .and_then(|home| {
            let workspace = std::env::current_dir().map_err(CliError::ExtensionDirectory)?;
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
) -> Result<i32, CliError> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let plan = plan_request(home, request, fiber_version, origin, clock)?;
    // The plan's binary downloads record on the one-shot writer at
    // debug; building it is best-effort and never fails the install.
    let writer = std::env::current_dir()
        .ok()
        .and_then(|workspace| crate::debug_writer::cli_writer(home, &workspace));
    let plan = plan.with_diagnostics(writer);
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
) -> Result<i32, CliError> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let listing = extensions::list(home, clock)?;
    // The member binaries record on the one-shot writer at debug;
    // building it is best-effort and never fails the update.
    let writer = std::env::current_dir()
        .ok()
        .and_then(|workspace| crate::debug_writer::cli_writer(home, &workspace));
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
        )
        .map(|plan| plan.with_diagnostics(writer.clone()))?;
        let code = report_install(commit_plan(home, None, plan, terminal, input, err), err)?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

fn remove(home: &Path, typed: &str, clock: &dyn Clock, io: Io<'_>) -> Result<i32, CliError> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let removal = extensions::removal(home, typed, clock)?;
    let approved = approve::remove_approved(&removal.names, &removal.data, terminal, input, err)?;
    if !approved {
        writeln!(err, "fiber: nothing was removed.").unwrap_or(());
        return Ok(1);
    }
    let names = removal.names.clone();
    removal.commit()?;
    for name in names {
        writeln!(err, "fiber: removed {name}").unwrap_or(());
    }
    Ok(0)
}

/// Whether the workspace's repository declares an extension: the project
/// key costs a `git` call, so it is computed only then (ruling 17). A
/// declaration that does not read still counts as declaring, so the
/// repository path reports the failure as today.
fn declares_extensions(workspace: &Path) -> bool {
    match config::declared(workspace) {
        Ok(declared) => !declared.extensions.is_empty(),
        Err(_) => true,
    }
}

/// The launch project's key, or `None` when the workspace declares no
/// extension and the repository path is skipped.
fn repository_project(
    home: &Path,
    workspace: &Path,
) -> Result<Option<config::ProjectKey>, CliError> {
    if !declares_extensions(workspace) {
        return Ok(None);
    }
    crate::project_of(home, workspace).map(|(_, project)| Some(project))
}

/// `fiber extension list` in `workspace`: the personal lines, then one
/// row per declared extension this project approved, at its current
/// content: name, version, `repository`, the project, the declared
/// path and the content hash. A declaration that does not read names
/// itself once and leaves the personal lines alone.
fn list_in(
    home: &Path,
    workspace: &Path,
    clock: &dyn Clock,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let Some(project) = repository_project(home, workspace)? else {
        return list(home, clock, out);
    };
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
                worktree::project(workspace).display(),
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
) -> Result<i32, CliError> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let project = repository_project(home, workspace)?;
    let found = match &project {
        Some(project) => match approved_extension(home, project, workspace, typed, clock) {
            Ok(found) => found,
            Err(e) => {
                writeln!(err, "fiber: {e}").unwrap_or(());
                None
            }
        },
        None => None,
    };
    let (Some(project), Some((item, digest))) = (project, found) else {
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
    extensions::Store::never_in(home, &project, &item, &digest)?;
    writeln!(
        err,
        "fiber: removed {} from this project; it is not offered again until its content changes",
        item.name
    )
    .unwrap_or(());
    Ok(0)
}

fn list(home: &Path, clock: &dyn Clock, out: &mut dyn Write) -> Result<(), CliError> {
    let listing = extensions::list(home, clock)?;
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
    result: Result<Option<Vec<String>>, CliError>,
    err: &mut dyn Write,
) -> Result<i32, CliError> {
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
) -> Result<extensions::Plan<'c>, CliError> {
    extensions::plan(home, &request, fiber_version, origin, clock).map_err(CliError::from)
}

pub(crate) fn summaries(plan: &extensions::Plan<'_>) -> Vec<approve::InstallSummary> {
    plan.items()
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
        .collect()
}

fn commit_plan(
    home: &Path,
    scope: Option<&ScopedInstall>,
    plan: extensions::Plan<'_>,
    terminal: bool,
    input: &mut dyn BufRead,
    err: &mut dyn Write,
) -> Result<Option<Vec<String>>, CliError> {
    let summaries = summaries(&plan);
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
            )?;
        }
        for name in plan.fresh_names() {
            config::set(
                home,
                &scope.workspace,
                &scope.project,
                config::Layer::Global,
                &format!("extensions.\"{}\".enabled", config::short_name(name)),
                serde_json::Value::Bool(false),
            )?;
        }
    }
    plan.commit().map(Some).map_err(CliError::from)
}

#[cfg(test)]
#[path = "extension_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "extension_repo_tests.rs"]
mod repo_tests;
