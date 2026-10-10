//! The installed models' lists, read the way `fiber models` reads them
//! (`docs/model-routing.md`, "Model discovery"). A [`ModelLists::Cached`]
//! read serves the cached lists and starts no extension;
//! [`ModelLists::Refresh`] loads the Lua providers, refreshes with or
//! without the age check, joins every refresh, and drops the extensions
//! before returning, so no VM stays resident.

use std::path::Path;
use std::sync::Arc;

use config::{Config, ModelData};
use contract::ErrorCode;
use contract::shapes::Failure;
use extensions::{Providers, SessionExtensions};

/// How the installed models' lists are read: [`model_lists`] on a thread
/// off the loop, with Fiber home and the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelLists {
    /// Serve the cached lists; start no extension.
    Cached,
    /// Load the Lua providers and refresh their lists, with the age check
    /// when `check_age` is set.
    Refresh {
        /// Whether lists fresher than `model_lists.refresh_after` are kept.
        check_age: bool,
    },
}

/// Why the installed models' lists could not be read: each case maps to
/// its stable code with no wildcard arm.
#[derive(Debug, thiserror::Error)]
pub enum ModelListsError {
    /// Loading the providers or the configuration failed: the failure as
    /// the caller built it.
    #[error("{}", .0.message)]
    Load(Failure),
    /// A cached list could not be read.
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    /// Filling the providers' placeholders failed.
    #[error(transparent)]
    Placeholders(#[from] extensions::Error),
}

impl ModelListsError {
    /// The stable code a caller switches on.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Load(failure) => failure.code.clone(),
            Self::Config(error) => error.code(),
            Self::Placeholders(error) => error.code(),
        }
    }
}

/// The installed providers, the configuration for `home` and `workspace`,
/// and the notices loading the providers gave, with every installed
/// model's list read at `read`: the cached lists at once, refreshed with
/// or without the age check for [`ModelLists::Refresh`]. `load` starts the
/// Lua providers; a [`ModelLists::Cached`] read never calls it. `Err`
/// names why nothing could be read; a failed refresh is a notice, and the
/// cached list stays.
pub fn model_lists(
    home: &Path,
    workspace: &Path,
    read: ModelLists,
    load: &dyn Fn(&Config) -> SessionExtensions,
) -> Result<(Providers, Config, Vec<String>), ModelListsError> {
    let (mut providers, config, load_notices) =
        crate::models::providers_and_config(home, workspace).map_err(ModelListsError::Load)?;
    let mut notices: Vec<String> = load_notices
        .into_iter()
        .map(|notice| notice.message)
        .collect();
    // The cached lists serve every cached copy, including a removed Lua
    // provider's, until a refresh answers without it. A refresh works
    // from the Lua providers alone, so a removed one's cache file shows
    // no longer once it answers.
    match read {
        ModelLists::Cached => {
            cached_lists(home, &mut providers, &mut notices)?;
        }
        ModelLists::Refresh { check_age } => {
            let max_age = check_age.then(|| config::refresh_after(&config));
            refreshed(&config, load, &mut providers, &mut notices, max_age);
        }
    }
    for notice in providers.fill_placeholders(&config, &|name| std::env::var(name).ok())? {
        notices.push(notice.message);
    }
    providers.forget_lua();
    Ok((providers, config, notices))
}

/// Adds every cached-only provider's list: each cached name no provider
/// holds, except `scripted`, which no installed package may register.
/// A name whose file no longer reads as a list is no copy at all; one
/// that cannot be read fails the read.
fn cached_lists(
    home: &Path,
    providers: &mut Providers,
    notices: &mut Vec<String>,
) -> Result<(), ModelListsError> {
    for name in config::cached_model_lists(home)? {
        if name == "scripted" || providers.get(&name).is_some() {
            continue;
        }
        let models = config::read_model_cache(home, &name)?.unwrap_or_default();
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
                // The caller exposes text, not notice extension metadata.
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

/// The role names marking `reference`: a role marks its model when its
/// value, a string or an object's `model`, is `fiber:<reference>` exactly,
/// or names it before a `:<level>` suffix. A role of another harness marks
/// nothing. Role names sorted.
pub fn roles_of(config: &Config, reference: &str) -> Vec<String> {
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
#[path = "model_lists_tests.rs"]
mod tests;
