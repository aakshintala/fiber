//! `fiber config get|set` (`docs/configuration.md`, "When Fiber writes";
//! `docs/invocation.md`, "Commands and flags"): `get` prints the effective
//! value and the layer it came from, `set` writes one key in one layer's
//! file. `main` parses argv and dispatches here; this crate takes plain
//! values.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::{Config, Layer, ProjectKey, Sources};
use contract::events::Notice;
use contract::shapes::Failure;
use extensions::{Providers, RepositorySet};
use serde_json::Value;

use crate::{CliError, project_of, with_dirs, write_line};

/// The installed providers merged with `workspace`'s approved repository
/// set for `project`, and the notices loading them gave: every personal
/// install loads, as `Providers::load` reads, with approved repository
/// entries merged in. The set loads read-only on the process clock, which
/// only bounds a contended lock wait; an empty set loads what `load` loads.
fn workspace_providers(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
) -> Result<(Providers, Vec<Notice>), CliError> {
    let set =
        RepositorySet::load_pinned(home, project, workspace, Arc::new(support::clock::System));
    Ok(Providers::load_all_repository(home, &set)?)
}

/// Prints each notice as one line on standard error: exactly the notice's
/// message, nothing else (`docs/configuration.md`, "When Fiber reads
/// configuration"). Standard output and the exit status are untouched.
pub(crate) fn print_notices(err: &mut dyn Write, notices: &[Notice]) {
    for notice in notices {
        writeln!(err, "{}", notice.message).unwrap_or(());
    }
}

/// The effective value of `key` and the layer it came from, as `get` prints
/// them: the value as compact JSON, then ` from `, then the layer.
fn run_get(
    home: &Path,
    workspace: &Path,
    key: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let (_, project) = project_of(home, workspace)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: Vec::new(),
    })?;
    // Each notice as one line on stderr: this load's, then the installed
    // providers' merged with the workspace's approved repository set,
    // each once per command run.
    let mut notices: Vec<Notice> = config.notices().to_vec();
    if let Ok((_, loading)) = workspace_providers(home, workspace, &project) {
        notices.extend(loading);
    }
    print_notices(err, &notices);
    // An extension's own setting never lives in `config.json`: it reads
    // from the extension's settings files, with the layer each value came
    // from, as `set` writes them.
    if let Some((extension, relative)) = config::settings_key(key) {
        return run_get_extension(
            home, workspace, &project, &config, key, &extension, &relative, out, err,
        );
    }
    // The notes print as the reviewer reads them: each layer's text under
    // its heading, with no layer named (`docs/permissions.md`, "What the
    // person tells it").
    if key == "reviewer.context" {
        let notes = config.reviewer_context();
        if notes.is_empty() {
            writeln!(err, "`{key}` is not set.").map_err(CliError::StandardError)?;
            return Ok(());
        }
        write_line(out, &notes).map_err(CliError::from)?;
        return Ok(());
    }
    match config.get(key, None) {
        Some((value, source)) => {
            let text = serde_json::to_string(&value).map_err(|source| CliError::ConfigValue {
                key: key.to_owned(),
                source,
            })?;
            write_line(out, &format!("{text} from {source}")).map_err(CliError::from)?;
            Ok(())
        }
        None if !config::known_key(key) => Err(CliError::ConfigKeyUnknown {
            key: key.to_owned(),
        }),
        None => {
            writeln!(err, "`{key}` is not set.").map_err(CliError::StandardError)?;
            Ok(())
        }
    }
}

/// The effective value of an extension's own setting and the settings file
/// it came from (`docs/configuration.md`, "Extension settings"). The
/// repository filter reads the manifest of the installed or approved copy:
/// the approved copy wins, the personal install is the fallback, and
/// neither is today's unknown key. The empty rest takes the unknown-key
/// path too, as `set` refuses it as usage.
fn run_get_extension(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    config: &Config,
    key: &str,
    extension: &str,
    relative: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    if relative.is_empty() {
        return Err(CliError::ConfigKeyUnknown {
            key: key.to_owned(),
        });
    }
    let dir = workspace_providers(home, workspace, project)
        .ok()
        .and_then(|(providers, _)| providers.extension_source_dir(extension))
        .or_else(|| config::installed_dir(home, extension));
    let Some(dir) = dir else {
        return Err(CliError::ConfigKeyUnknown {
            key: key.to_owned(),
        });
    };
    let declared = config::declared_settings_in(&dir)?;
    let repo: Vec<&str> = declared.repo_settings.iter().map(String::as_str).collect();
    match config
        .extensions()
        .get_layered(extension, &repo, &config::path::display(relative))?
    {
        Some((value, source)) => {
            let text = serde_json::to_string(&value).map_err(|source| CliError::ConfigValue {
                key: key.to_owned(),
                source,
            })?;
            write_line(out, &format!("{text} from {source}")).map_err(CliError::from)?;
            Ok(())
        }
        None => {
            writeln!(err, "`{key}` is not set.").map_err(CliError::StandardError)?;
            Ok(())
        }
    }
}

/// Writes one key in one layer's file, parsing the value as JSON, or a bare
/// string when it does not parse, as `-c` does. On success prints each
/// loading notice as one line on standard error, each load's once per
/// command run; nothing else.
fn run_set(
    home: &Path,
    workspace: &Path,
    layer: Layer,
    key: &str,
    value: &str,
    err: &mut dyn Write,
    clock: &dyn contract::clock::Clock,
) -> Result<PathBuf, CliError> {
    let (_, project) = project_of(home, workspace)?;
    let parsed = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.into()));
    let mut notices = Vec::new();
    if key == "model"
        && let Some(typed) = parsed.as_str()
    {
        notices.extend(check_model(home, workspace, &project, typed, err, clock)?);
    } else {
        // No other load happens on this path: one lenient load surfaces
        // this command's configuration notices. A configuration that
        // fails to load fails no set.
        if let Ok(config) = Config::load(Sources {
            home: home.to_path_buf(),
            workspace: workspace.to_path_buf(),
            project: project.clone(),
            overrides: Vec::new(),
        }) {
            notices.extend(config.notices().iter().cloned());
        }
    }
    // A repository-layer settings key resolves the approved-or-installed
    // directory the caller alone may resolve — the approved copy wins,
    // the personal install is the fallback — and writes through the narrow
    // settings API. Global and project keys fall through to `set`, which
    // resolves the installed copy internally, and a key no copy resolves
    // falls through to `set`'s refusal.
    if matches!(layer, Layer::Repository)
        && let Some((extension, _)) = config::settings_key(key)
        && let Some(dir) = workspace_providers(home, workspace, &project)
            .ok()
            .and_then(|(providers, _)| providers.extension_source_dir(&extension))
            .or_else(|| config::installed_dir(home, &extension))
    {
        let file =
            config::set_repo_extension_setting(home, workspace, &project, &dir, key, parsed)?;
        print_notices(err, &notices);
        return Ok(file);
    }
    let file = config::set(home, workspace, &project, layer, key, parsed)?;
    print_notices(err, &notices);
    Ok(file)
}

/// `fiber config set` for another front end, such as the terminal's
/// `/settings`: the same parsing, type check, layer refusal and `model`
/// check, with each warning line `set` prints on standard error returned
/// instead, and the file `set` wrote.
pub fn config_set_text(
    home: &Path,
    workspace: &Path,
    layer: Layer,
    key: &str,
    text: &str,
    clock: &dyn contract::clock::Clock,
) -> Result<(Vec<String>, PathBuf), Failure> {
    let mut err = Vec::new();
    let file =
        run_set(home, workspace, layer, key, text, &mut err, clock).map_err(Failure::from)?;
    Ok((
        String::from_utf8_lossy(&err)
            .lines()
            .map(str::to_owned)
            .collect(),
        file,
    ))
}

/// Checks a `model` value against the installed providers' cached model
/// lists merged with the workspace's approved repository set, by the rules in `docs/model-routing.md`, "Naming a model"
/// (`docs/configuration.md`, "When Fiber writes"). With no cached list
/// the value is accepted, and no network is touched for any key. The typed
/// text is written as typed, never normalised. Returns every notice the
/// check's loads raised, for the caller to print once per command run.
fn check_model(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    typed: &str,
    err: &mut dyn Write,
    clock: &dyn contract::clock::Clock,
) -> Result<Vec<Notice>, CliError> {
    let (providers, loading) = workspace_providers(home, workspace, project)?;
    let mut notices = loading;
    // Without a cached list the reference is accepted unchecked: one
    // naming a provider that is not installed, or one installed with an
    // empty list, and a bare id when every installed list is empty. This
    // runs on the unfilled load, so a provider whose every model is
    // unconfigured still counts as cached.
    let (base, _) = Providers::split_thinking(typed);
    let uncached = match base.split_once('/') {
        Some((name, _)) => providers
            .get(name)
            .is_none_or(|provider| provider.models.is_empty()),
        None => providers.names().all(|name| {
            providers
                .get(name)
                .is_some_and(|provider| provider.models.is_empty())
        }),
    };
    if uncached {
        // No cached list to check against, so no strict load happens on
        // this path: one lenient load still surfaces this command's
        // configuration notices. A configuration that fails to load fails
        // no set.
        if let Ok(config) = Config::load(Sources {
            home: home.to_path_buf(),
            workspace: workspace.to_path_buf(),
            project: project.clone(),
            overrides: Vec::new(),
        }) {
            notices.extend(config.notices().iter().cloned());
        }
        return Ok(notices);
    }
    // The fill reads the person's own files with no overrides, and the
    // process environment, touching no network: the person may set the
    // host next, so a reference left out with `model_unconfigured` warns
    // and is written. Notices for other left-out models are dropped.
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: Vec::new(),
    })
    .map_err(CliError::from)?;
    notices.extend(config.notices().iter().cloned());
    let mut providers = providers;
    let lookup = crate::hub_env::placeholder_lookup(home, &providers, clock);
    let _notices = providers.fill_placeholders(&config, &lookup)?;
    match providers.resolve(typed) {
        Ok(_) => Ok(notices),
        Err(e @ extensions::Error::Unconfigured { .. }) => {
            writeln!(err, "fiber: {e}").unwrap_or(());
            Ok(notices)
        }
        Err(extensions::Error::UnknownModel { .. } | extensions::Error::ModelMissing { .. }) => {
            Err(no_model(&providers, typed))
        }
        Err(e @ extensions::Error::Ambiguous { .. }) => Err(CliError::ModelAmbiguous(e)),
        Err(e) => Err(CliError::from(e)),
    }
}

/// The `no_model` failure: what was typed, the closest installed
/// references, and where to list them.
fn no_model(providers: &Providers, typed: &str) -> CliError {
    CliError::ModelNone {
        typed: typed.to_owned(),
        closest: closest(providers, typed),
    }
}

/// Up to 3 installed `provider/model` references nearest the typed text,
/// with any `:<level>` suffix stripped, nearest first and ties in sorted
/// reference order.
fn closest(providers: &Providers, typed: &str) -> Vec<String> {
    let (base, _) = Providers::split_thinking(typed);
    let mut refs: Vec<String> = providers
        .names()
        .flat_map(|name| {
            providers.get(name).into_iter().flat_map(move |provider| {
                provider
                    .models
                    .iter()
                    .map(move |model| format!("{name}/{}", model.id))
            })
        })
        .collect();
    refs.sort();
    refs.sort_by_key(|candidate| distance(base, candidate));
    refs.truncate(3);
    refs
}

/// Edits between two references, in characters, ranking the closest matches.
fn distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut prev: Vec<usize> = (0..=right.len()).collect();
    for (i, l) in left.iter().enumerate() {
        let mut current = Vec::with_capacity(right.len() + 1);
        current.push(i + 1);
        for (j, r) in right.iter().enumerate() {
            let cost = usize::from(l != r);
            let above = prev.get(j + 1).map_or(usize::MAX, |v| v.saturating_add(1));
            let beside = prev.get(j).map_or(usize::MAX, |v| v.saturating_add(cost));
            let side = current.get(j).map_or(usize::MAX, |v| v.saturating_add(1));
            current.push(above.min(beside).min(side));
        }
        prev = current;
    }
    prev.get(right.len()).copied().unwrap_or(usize::MAX)
}

/// `fiber config get <key>` in the current directory: prints the effective
/// value and the layer it came from.
pub fn config_get(key: &str) -> i32 {
    with_dirs(|home, workspace| run_get(home, workspace, key, &mut io::stdout(), &mut io::stderr()))
}

/// `fiber config set [--project | --repo] <key> <value>` in the current
/// directory: writes one key in one layer's file.
pub fn config_set(layer: Layer, key: &str, value: &str, clock: &dyn contract::clock::Clock) -> i32 {
    with_dirs(|home, workspace| {
        run_set(home, workspace, layer, key, value, &mut io::stderr(), clock).map(|_| ())
    })
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "config_extension_tests.rs"]
mod config_extension_tests;
