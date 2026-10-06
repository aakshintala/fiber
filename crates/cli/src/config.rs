//! `fiber config get|set` (`docs/configuration.md`, "When Fiber writes";
//! `docs/invocation.md`, "Commands and flags"): `get` prints the effective
//! value and the layer it came from, `set` writes one key in one layer's
//! file. `main` parses argv and dispatches here; this crate takes plain
//! values.

use std::io::{self, Write};
use std::path::Path;

use config::{Config, Layer, Sources};
use contract::ErrorCode;
use contract::shapes::Failure;
use extensions::Providers;
use serde_json::Value;

use crate::{fail, failed, project_of};

/// The effective value of `key` and the layer it came from, as `get` prints
/// them: the value as compact JSON, then ` from `, then the layer.
fn run_get(
    home: &Path,
    workspace: &Path,
    key: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Failure> {
    let (_, project) = project_of(home, workspace)?;
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides: Vec::new(),
    })
    .map_err(|e| failed(e.code(), e))?;
    match config.get(key, None) {
        Some((value, source)) => {
            let text = serde_json::to_string(&value)
                .map_err(|e| failed(ErrorCode::IoFailed, format!("a value for `{key}`: {e}")))?;
            writeln!(out, "{text} from {source}")
                .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))?;
            Ok(())
        }
        None if !config::known_key(key) => Err(crate::usage(format!(
            "`{key}` is not a configuration key. Run `fiber --help` for usage."
        ))),
        None => {
            writeln!(err, "`{key}` is not set.")
                .map_err(|e| failed(ErrorCode::IoFailed, format!("standard error: {e}")))?;
            Ok(())
        }
    }
}

/// Writes one key in one layer's file, parsing the value as JSON, or a bare
/// string when it does not parse, as `-c` does. Prints nothing on success.
fn run_set(
    home: &Path,
    workspace: &Path,
    layer: Layer,
    key: &str,
    value: &str,
) -> Result<(), Failure> {
    let (_, project) = project_of(home, workspace)?;
    let parsed = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.into()));
    if key == "model"
        && let Some(typed) = parsed.as_str()
    {
        check_model(home, typed)?;
    }
    config::set(home, workspace, &project, layer, key, parsed).map_err(|e| failed(e.code(), e))
}

/// The thinking levels a typed model may end in, after a `:`
/// (`docs/model-routing.md`, "Naming a model"), mirroring the list
/// `Providers::resolve` matches against.
const THINKING: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// A typed model reference without any `:<level>` suffix.
fn strip_thinking(typed: &str) -> &str {
    typed
        .rsplit_once(':')
        .and_then(|(rest, level)| THINKING.contains(&level).then_some(rest))
        .unwrap_or(typed)
}

/// Checks a `model` value against the installed providers' cached model
/// lists, by the rules in `docs/model-routing.md`, "Naming a model"
/// (`docs/configuration.md`, "When Fiber writes"). With no cached list
/// the value is accepted, and no network is touched for any key. The typed
/// text is written as typed, never normalised.
fn check_model(home: &Path, typed: &str) -> Result<(), Failure> {
    // debt: notices from loading are dropped, as `parts_with` drops them;
    // surfaced when #382 lands.
    let (providers, _notices) = Providers::load(home).map_err(|e| failed(e.code(), e))?;
    // Without a cached list the reference is accepted unchecked: one
    // naming a provider that is not installed, or one installed with an
    // empty list, and a bare id when every installed list is empty.
    let base = strip_thinking(typed);
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
        return Ok(());
    }
    match providers.resolve(typed) {
        Ok(_) => Ok(()),
        Err(extensions::Error::UnknownModel { .. } | extensions::Error::ModelMissing { .. }) => {
            Err(no_model(&providers, typed))
        }
        Err(e @ extensions::Error::Ambiguous { .. }) => {
            Err(failed(ErrorCode::ModelAmbiguous, e.to_string()))
        }
        Err(e) => Err(failed(e.code(), e.to_string())),
    }
}

/// The `no_model` failure: what was typed, the closest installed
/// references, and where to list them.
fn no_model(providers: &Providers, typed: &str) -> Failure {
    let near: Vec<String> = closest(providers, typed);
    let mut message = format!("No installed model matches `{typed}`.");
    if !near.is_empty() {
        let listed = near
            .iter()
            .map(|reference| format!("`{reference}`"))
            .collect::<Vec<_>>()
            .join(", ");
        message.push_str(&format!(" Closest: {listed}."));
    }
    message.push_str(" Run `fiber models` to list them.");
    failed(ErrorCode::NoModel, message)
}

/// Up to 3 installed `provider/model` references nearest the typed text,
/// with any `:<level>` suffix stripped, nearest first and ties in sorted
/// reference order.
fn closest(providers: &Providers, typed: &str) -> Vec<String> {
    let base = strip_thinking(typed);
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
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            run_get(&home, &workspace, key, &mut io::stdout(), &mut io::stderr())
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// `fiber config set [--project | --repo] <key> <value>` in the current
/// directory: writes one key in one layer's file.
pub fn config_set(layer: Layer, key: &str, value: &str) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let workspace = std::env::current_dir()
                .map_err(|e| failed(ErrorCode::IoFailed, format!("the current directory: {e}")))?;
            run_set(&home, &workspace, layer, key, value)
        });
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
