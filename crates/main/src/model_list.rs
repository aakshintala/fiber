//! The installed models for the terminal's model picker (`docs/tui.md`,
//! "Swapped views"): one entry per model of every installed provider,
//! read the way `fiber models` reads them (`docs/model-routing.md`,
//! "Model discovery"). A `Cached` read serves the cached lists and starts
//! no extension; `Stale` and `Every` load the Lua providers, refresh with
//! and without the age check, join every refresh, and drop the extensions
//! before returning, so no VM stays resident and the idle terminal starts
//! none.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use config::{Config, ModelData};
use contract::clock::Clock;
use contract::files::PathLock;
use extensions::{Providers, SessionExtensions};

/// How the terminal reads the installed models: [`read`] on a thread off
/// the loop, with Fiber home, the workspace, the clock and the locks.
pub(crate) fn reader(
    home: PathBuf,
    workspace: PathBuf,
    clock: Arc<dyn Clock>,
    locks: Arc<dyn PathLock>,
) -> tui::ReadModels {
    Arc::new(move |refresh| {
        read(&home, &workspace, refresh, &|config| {
            SessionExtensions::load(&home, config, Arc::clone(&clock), Arc::clone(&locks), None)
        })
    })
}

/// The installed models at `refresh`: the cached lists at once, refreshed
/// in the background for `Stale` and `Every`. `load` starts the Lua
/// providers; a `Cached` read never calls it. `Err` names why nothing
/// could be read; a failed refresh is a notice, and the cached list stays.
pub(crate) fn read(
    home: &Path,
    workspace: &Path,
    refresh: tui::Refresh,
    load: &dyn Fn(&Config) -> SessionExtensions,
) -> Result<tui::Catalogue, String> {
    let (mut providers, config, load_notices) =
        cli::providers_and_config(home, workspace).map_err(|failure| failure.message)?;
    let mut notices: Vec<String> = load_notices
        .into_iter()
        .map(|notice| notice.message)
        .collect();
    // The cached lists serve every cached copy, including a removed Lua
    // provider's, until a `Stale` read answers without it. A refresh works
    // from the Lua providers alone, so a removed one's cache file shows
    // no longer once it answers.
    match refresh {
        tui::Refresh::Cached => {
            cached_lists(home, &mut providers, &mut notices)?;
        }
        tui::Refresh::Stale | tui::Refresh::Every => {
            let max_age = (refresh == tui::Refresh::Stale).then(|| config::refresh_after(&config));
            refreshed(&config, load, &mut providers, &mut notices, max_age);
        }
    }
    for notice in providers
        .fill_placeholders(&config, &|name| std::env::var(name).ok())
        .map_err(|error| error.to_string())?
    {
        notices.push(notice.message);
    }
    providers.forget_lua();
    Ok(tui::Catalogue {
        models: entries(&providers, &config),
        notices,
    })
}

/// Adds every cached-only provider's list: each cached name no provider
/// holds, except `scripted`, which no installed package may register.
/// A name whose file no longer reads as a list is no copy at all; one
/// that cannot be read fails the read.
fn cached_lists(
    home: &Path,
    providers: &mut Providers,
    notices: &mut Vec<String>,
) -> Result<(), String> {
    for name in config::cached_model_lists(home).map_err(|error| error.to_string())? {
        if name == "scripted" || providers.get(&name).is_some() {
            continue;
        }
        let models = config::read_model_cache(home, &name)
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let mut models = models;
        notices.extend(
            extensions::leave_out_invalid(&name, &name, &mut models)
                .into_iter()
                .map(|notice| notice.message),
        );
        providers.add_cached(&name, models);
    }
    Ok(())
}

/// Loads the Lua providers, refreshes them with `max_age`, joins every
/// refresh, and drops the extensions before returning, so no VM stays
/// resident. A joined list replaces its provider's; a failed refresh is
/// one notice, and the cached list stays.
fn refreshed(
    config: &Config,
    load: &dyn Fn(&Config) -> SessionExtensions,
    providers: &mut Providers,
    notices: &mut Vec<String>,
    max_age: Option<std::time::Duration>,
) {
    let loaded = load(config);
    for (extension, provider) in loaded.lua_providers() {
        notices.extend(
            providers
                .add_lua(extension, provider, config)
                .into_iter()
                .map(|notice| notice.message),
        );
    }
    let wanted: Vec<Arc<extensions::LuaProvider>> = loaded
        .lua_providers()
        .iter()
        .map(|(_, provider)| Arc::clone(provider))
        .collect();
    for (name, handle) in extensions::refresh_lists(&wanted, providers, config, max_age) {
        match handle.join() {
            Ok(Ok(models)) => {
                let mut models: Vec<ModelData> = models;
                // The catalogue exposes text, not notice extension metadata.
                notices.extend(
                    extensions::leave_out_invalid(&name, &name, &mut models)
                        .into_iter()
                        .map(|notice| notice.message),
                );
                providers.set_models(&name, models);
            }
            Ok(Err(error)) => {
                notices.push(format!("Could not refresh {name}'s model list: {error}"));
            }
            Err(_) => {
                notices.push(format!(
                    "Could not refresh {name}'s model list: the refresh did not answer."
                ));
            }
        }
    }
}

/// One entry per model of every installed provider, providers sorted by
/// name, models in list order.
fn entries(providers: &Providers, config: &Config) -> Vec<tui::ModelEntry> {
    let mut entries = Vec::new();
    for name in providers.names() {
        let Some(data) = providers.get(name) else {
            continue;
        };
        for model in &data.models {
            let reference = format!("{name}/{}", model.id);
            entries.push(tui::ModelEntry {
                reference: reference.clone(),
                provider: name.to_owned(),
                id: model.id.clone(),
                levels: model
                    .thinking_levels
                    .iter()
                    .map(|level| level.as_str().to_owned())
                    .collect(),
                default_level: model
                    .thinking_default
                    .map(|level| level.as_str().to_owned()),
                configured: config
                    .get("thinking", Some(&reference))
                    .and_then(|(value, _)| value.as_str().map(str::to_owned)),
                roles: roles_of(config, &reference),
            });
        }
    }
    entries
}

/// The role names marking `reference`: a role marks its model when its
/// value, a string or an object's `model`, is `fiber:<reference>` exactly,
/// or names it before a `:<level>` suffix. A role of another harness marks
/// nothing. Role names sorted.
pub(crate) fn roles_of(config: &Config, reference: &str) -> Vec<String> {
    let wanted = format!("fiber:{reference}");
    let roles = config.merged(None);
    let Some(roles) = roles.get("roles").and_then(|roles| roles.as_object()) else {
        return Vec::new();
    };
    let mut names: Vec<&String> = roles.keys().collect();
    names.sort();
    let mut marked = Vec::new();
    for name in names {
        let model = match roles.get(name) {
            Some(serde_json::Value::String(text)) => Some(text.as_str()),
            Some(serde_json::Value::Object(object)) => {
                object.get("model").and_then(|model| model.as_str())
            }
            _ => None,
        };
        let Some(model) = model else {
            continue;
        };
        if model == wanted {
            marked.push(name.clone());
            continue;
        }
        let (rest, level) = Providers::split_thinking(model);
        if level.is_some() && rest == wanted {
            marked.push(name.clone());
        }
    }
    marked
}

#[cfg(test)]
#[path = "model_list_tests.rs"]
mod tests;
