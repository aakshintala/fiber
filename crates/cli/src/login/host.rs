//! The login host step (`docs/model-routing.md`, "A per-account host"):
//! after a credential is stored, each presence gap is asked for at a
//! terminal and saved globally, before the reviewer is asked. A gap is
//! presence, never validity: a set-but-invalid value asks nothing, and
//! the model list still leaves the model out with `model_unconfigured`.
//! On a pipe one remedy line per gap is printed instead, and the stored
//! line follows the remedy lines. Only the prompt reads the terminal;
//! the saves never do.

use std::fs;
use std::path::{Path, PathBuf};

use config::{Config, ProjectKey, Scope, Sources, settings_file};
use extensions::Providers;
use serde_json::Value;

use super::LoginIo;
use crate::CliError;

/// The host questions for provider `name`: at a terminal each presence
/// gap is asked for and saved globally; on a pipe one remedy line per
/// gap is printed. Always before the stored line, never after. An
/// unreadable working directory, an unreadable configuration, or a failed
/// placeholder lookup fails the login: a broken config never reads as no
/// gaps with a success report.
pub(super) fn hosts(io: &mut LoginIo<'_>, name: &str) -> Result<(), CliError> {
    let (workspace, project) = project_of(io.home)?;
    if io.terminal {
        ask_all(io, name, &workspace, &project)
    } else {
        remedies(io, name, &workspace, &project)
    }
}

/// The working directory's project: an unreadable directory or project
/// fails the login instead of skipping its host step.
fn project_of(home: &Path) -> Result<(PathBuf, ProjectKey), CliError> {
    let workspace = std::env::current_dir().map_err(CliError::CurrentDirectory)?;
    let (_, project) = crate::project_of(home, &workspace)?;
    Ok((workspace, project))
}

/// The configuration over `home` for `workspace`'s project: a broken
/// configuration fails the login instead of skipping its host step.
fn load(home: &Path, workspace: &Path, project: &ProjectKey) -> Result<Config, CliError> {
    Ok(Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: project.clone(),
        overrides: Vec::new(),
    })?)
}

/// The process environment: present-but-invalid reads as present, since
/// gaps are presence.
fn environment(var: &str) -> Option<String> {
    std::env::var(var).ok()
}

/// Provider `name`'s presence gaps in declaration order: each setting
/// with its declared environment variable. A failed lookup fails the
/// login instead of reading as no gaps.
fn gaps(
    home: &Path,
    providers: &Providers,
    name: &str,
    workspace: &Path,
    project: &ProjectKey,
) -> Result<Vec<(String, Option<String>)>, CliError> {
    let config = load(home, workspace, project)?;
    Ok(providers.unset_placeholders(&config, &environment, name)?)
}

/// Asks for every presence gap of provider `name` in declaration order,
/// saving each answer globally before asking the next: each save is
/// re-queried, so an answered gap never asks again, and a saved gap that
/// still reads empty is a project-layer empty hiding it. End of input
/// ends the questions with nothing saved for any remaining gap; an empty
/// typed line is refused and asked again.
fn ask_all(
    io: &mut LoginIo<'_>,
    name: &str,
    workspace: &Path,
    project: &ProjectKey,
) -> Result<(), CliError> {
    // Every answer saved this run: a project-layer empty hides a saved
    // gap from the re-query, so it asks once and never again.
    let mut answered: Vec<String> = Vec::new();
    loop {
        let Some((key, env)) = gaps(io.home, io.providers, name, workspace, project)?
            .into_iter()
            .find(|(key, _)| !answered.contains(key))
        else {
            return Ok(());
        };
        if ask(io, name, &key, env.as_deref())?.is_ended() {
            return Ok(());
        }
        answered.push(key);
    }
}

/// One prompt's outcome: the answer validated and saved, or end of input.
enum Asked {
    /// The typed answer validated and saved globally.
    Saved,
    /// End of input: the questions end with nothing saved.
    Ended,
}

impl Asked {
    /// Whether the questions end.
    fn is_ended(&self) -> bool {
        matches!(self, Self::Ended)
    }
}

/// Asks for one gap: the setting with its declared variable. The answer
/// saves globally through [`save_host`], so the prompt and the save can
/// never disagree on what a host is. A project-layer empty masking the
/// saved answer names its file after the save, since the answer holds
/// for every other project and session.
fn ask(io: &mut LoginIo<'_>, name: &str, key: &str, env: Option<&str>) -> Result<Asked, CliError> {
    loop {
        match env {
            Some(variable) => write!(io.err, "`{key}` (`{variable}`): "),
            None => write!(io.err, "`{key}`: "),
        }
        .map_err(CliError::Terminal)?;
        let mut line = String::new();
        let bytes = io
            .stdin
            .read_line(&mut line)
            .map_err(CliError::StandardInput)?;
        if bytes == 0 {
            return Ok(Asked::Ended);
        }
        let answer = line.trim();
        if Providers::validate_host(answer).is_none() {
            writeln!(io.err, "`{key}` is not a host; press Ctrl-D to skip.")
                .map_err(CliError::Terminal)?;
            continue;
        }
        let masked = save_host(io.home, io.providers, name, key, answer)?;
        if let Some(file) = masked {
            writeln!(
                io.err,
                "fiber: saved `{key}` globally, but {} sets it empty and hides it.",
                file.display(),
            )
            .map_err(CliError::Terminal)?;
        }
        return Ok(Asked::Saved);
    }
}

/// Prints one remedy line per presence gap of provider `name`: the
/// setting with the config file that fills it, and the declared variable
/// when one exists.
fn remedies(
    io: &mut LoginIo<'_>,
    name: &str,
    workspace: &Path,
    project: &ProjectKey,
) -> Result<(), CliError> {
    let extension = io.providers.extension_of(name).unwrap_or(name);
    let file = config::dir_name(extension);
    for (key, env) in gaps(io.home, io.providers, name, workspace, project)? {
        match env {
            Some(variable) => writeln!(
                io.err,
                "fiber: `{key}` has no value; set it in `config/{file}.json` in Fiber home, \
                 or export `{variable}`."
            ),
            None => writeln!(
                io.err,
                "fiber: `{key}` has no value and no environment variable fills it; \
                 set it in `config/{file}.json` in Fiber home."
            ),
        }
        .map_err(CliError::Terminal)?;
    }
    Ok(())
}

/// Saves `value` as `key`'s answer for provider `name`: one global write
/// to the extension's settings file, the same file `fiber config set`
/// writes. A value that is not a host is refused with a usage error
/// naming the setting, never the value, and nothing is written. Returns
/// the project settings file hiding the saved value, when a
/// project-layer empty masks it.
pub fn save_host(
    home: &Path,
    providers: &Providers,
    name: &str,
    key: &str,
    value: &str,
) -> Result<Option<PathBuf>, CliError> {
    let Some(host) = Providers::validate_host(value) else {
        return Err(CliError::HostNotHost {
            name: key.to_owned(),
        });
    };
    // The save reports the hiding project file, so it reads the working
    // directory's project.
    let workspace = std::env::current_dir().map_err(CliError::CurrentDirectory)?;
    let (_, project) = crate::project_of(home, &workspace)?;
    let extension = providers.extension_of(name).unwrap_or(name);
    let mut config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.clone(),
        project: project.clone(),
        overrides: Vec::new(),
    })
    .map_err(CliError::Config)?;
    config
        .extensions_mut()
        .set(
            extension,
            Scope::Machine,
            key,
            Value::String(host.to_owned()),
        )
        .map_err(CliError::Config)?;
    Ok(masked_by_project(home, &project, extension, key))
}

/// The project settings file whose empty value hides the saved global
/// `key`, when the project layer sets it empty. `key` is a dotted key in
/// the `path::parse` syntax the settings reader uses, so a nested empty
/// project value still reads as the mask.
fn masked_by_project(
    home: &Path,
    project: &ProjectKey,
    extension: &str,
    key: &str,
) -> Option<PathBuf> {
    let file = settings_file(&home.join("projects").join(project.as_str()), extension);
    let bytes = fs::read(&file).ok()?;
    let json: Value = serde_json::from_slice(&bytes).ok()?;
    let segments = config::path::parse(key)?;
    let mut value = &json;
    for segment in &segments {
        value = value.get(segment)?;
    }
    if let Value::String(empty) = value
        && empty.is_empty()
    {
        Some(file)
    } else {
        None
    }
}
