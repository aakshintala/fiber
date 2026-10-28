//! The installed models' lists, read the way `fiber models` reads them
//! (`docs/model-routing.md`, "Model discovery"). A [`ModelLists::Cached`]
//! read serves the cached lists and starts no extension;
//! [`ModelLists::Refresh`] loads the Lua providers, refreshes with or
//! without the age check, joins every refresh, and drops the extensions
//! before returning, so no VM stays resident.

use std::path::Path;
use std::sync::Arc;

use config::Config;
use contract::ErrorCode;
use extensions::{Providers, SessionExtensions, Unconfigured};

use contract::events::Notice;

use crate::CliError;

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
    /// Loading the providers or the configuration failed: the error as
    /// the caller built it.
    #[error(transparent)]
    Load(CliError),
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
            Self::Load(error) => error.code(),
            Self::Config(error) => error.code(),
            Self::Placeholders(error) => error.code(),
        }
    }
}

/// The installed providers, the configuration for `home` and `workspace`,
/// and the [`Notice`]s loading the providers gave, with every installed
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
    run: config::Runner<'_>,
    clock: &dyn contract::clock::Clock,
) -> Result<(Providers, Config, Vec<Notice>), ModelListsError> {
    let (providers, config, mut notices, unconfigured) =
        model_lists_apart(home, workspace, read, load, run, clock)?;
    notices.extend(unconfigured);
    Ok((providers, config, notices))
}

/// [`model_lists`] with the [`Notice`]s of each model the placeholder fill
/// leaves out (`model_unconfigured`) apart, as the fourth value in the
/// fill's order: the third holds every other notice.
pub(crate) fn model_lists_apart(
    home: &Path,
    workspace: &Path,
    read: ModelLists,
    load: &dyn Fn(&Config) -> SessionExtensions,
    run: config::Runner<'_>,
    clock: &dyn contract::clock::Clock,
) -> Result<(Providers, Config, Vec<Notice>, Vec<Notice>), ModelListsError> {
    let (mut providers, config, notices, lookup) =
        lists_before_fill(home, workspace, read, load, run, clock)?;
    let unconfigured = providers.fill_placeholders(&config, &lookup)?;
    providers.forget_lua();
    Ok((providers, config, notices, unconfigured))
}

/// [`model_lists`] with the placeholder fill's two outcomes apart: the
/// fourth value holds the per-model notices for a value that is not a
/// host, and the fifth one group per provider and unset setting for a
/// placeholder with no value, in provider then setting order. The third
/// holds every other notice; a provider with no value is absent from both
/// notice lists.
#[allow(
    clippy::type_complexity,
    reason = "the grouped read: five values the terminal maps to its catalogue in one step"
)]
pub fn model_lists_grouped(
    home: &Path,
    workspace: &Path,
    read: ModelLists,
    load: &dyn Fn(&Config) -> SessionExtensions,
    run: config::Runner<'_>,
    clock: &dyn contract::clock::Clock,
) -> Result<
    (
        Providers,
        Config,
        Vec<Notice>,
        Vec<Notice>,
        Vec<Unconfigured>,
    ),
    ModelListsError,
> {
    let (mut providers, config, notices, lookup) =
        lists_before_fill(home, workspace, read, load, run, clock)?;
    let (not_host, groups) = providers.fill_placeholders_grouped(&config, &lookup)?;
    providers.forget_lua();
    Ok((providers, config, notices, not_host, groups))
}

/// The installed providers, the configuration for `home` and
/// `workspace`, and the [`Notice`]s loading the providers gave, with
/// every installed model's list read at `read`: the cached lists at
/// once, refreshed with or without the age check for
/// [`ModelLists::Refresh`]. `load` starts the Lua providers; a
/// [`ModelLists::Cached`] read never calls it. The fourth value reads
/// each placeholder value the way the hub would: from the hub's captured
/// environment, else the process environment.
#[allow(
    clippy::type_complexity,
    reason = "the shared read prefix: four values both the apart and grouped reads map in one step"
)]
fn lists_before_fill(
    home: &Path,
    workspace: &Path,
    read: ModelLists,
    load: &dyn Fn(&Config) -> SessionExtensions,
    run: config::Runner<'_>,
    clock: &dyn contract::clock::Clock,
) -> Result<
    (
        Providers,
        Config,
        Vec<Notice>,
        impl Fn(&str) -> Option<String> + use<>,
    ),
    ModelListsError,
> {
    let (mut providers, config, mut notices) =
        crate::models::providers_and_config(home, workspace).map_err(ModelListsError::Load)?;
    // The cached lists serve every cached copy an installed extension
    // declares. A refresh works from the Lua providers alone, so a
    // removed one's cache file shows no longer once it answers.
    // Each placeholder value from the hub's captured environment, so the
    // list agrees with the session the hub would start; no hub answers
    // and the process environment serves, as before.
    let lookup = crate::hub_env::placeholder_lookup(home, &providers, clock);
    match read {
        ModelLists::Cached => {
            cached_lists(home, &mut providers, &mut notices)?;
        }
        ModelLists::Refresh { check_age } => {
            let max_age = check_age.then(|| config::refresh_after(&config));
            refreshed(
                &config,
                load,
                &mut providers,
                &mut notices,
                max_age,
                run,
                &lookup,
            );
        }
    }
    Ok((providers, config, notices, lookup))
}

/// Adds every cached-only provider's list: each cached name no provider
/// holds, except `scripted`, which no installed package may register.
/// A name whose file no longer reads as a list is no copy at all; one
/// that cannot be read fails the read. A name no installed extension
/// declares is not listed.
fn cached_lists(
    home: &Path,
    providers: &mut Providers,
    notices: &mut Vec<Notice>,
) -> Result<(), ModelListsError> {
    for name in config::cached_model_lists(home)? {
        if name == "scripted" || providers.get(&name).is_some() {
            continue;
        }
        let models = config::read_model_cache(home, &name)?.unwrap_or_default();
        if let Some(admitted) = providers.add_cached(&name, models) {
            notices.extend(admitted);
        }
    }
    Ok(())
}

/// Loads the Lua providers, refreshes them with `max_age`, joins every
/// refresh, and drops the extensions before returning, so no VM stays
/// resident. A joined list replaces its provider's; a failed refresh is
/// one [`Notice`] with code `extension_failed` naming the extension, and
/// the cached list stays.
fn refreshed(
    config: &Config,
    load: &dyn Fn(&Config) -> SessionExtensions,
    providers: &mut Providers,
    notices: &mut Vec<Notice>,
    max_age: Option<std::time::Duration>,
    run: config::Runner<'_>,
    lookup: &dyn Fn(&str) -> Option<String>,
) {
    let loaded = load(config);
    notices.extend(providers.add_session(&loaded, config, run, lookup));
    // A `StartedRefresh` names the provider; the notice names the
    // extension that registered it (`docs/model-routing.md`, "Model
    // discovery": an `extension_failed` notice names the extension).
    let extension_of: std::collections::BTreeMap<&str, &str> = loaded
        .lua_providers()
        .iter()
        .map(|(extension, provider)| (provider.name(), extension.as_str()))
        .collect();
    let wanted: Vec<Arc<extensions::LuaProvider>> = loaded
        .lua_providers()
        .iter()
        .map(|(_, provider)| Arc::clone(provider))
        .collect();
    for (name, handle) in
        extensions::refresh_lists(&wanted, providers, config, max_age, run, lookup)
    {
        let extension = extension_of
            .get(name.as_str())
            .copied()
            .unwrap_or(name.as_str())
            .to_owned();
        match handle.join() {
            Ok(Ok(models)) => {
                notices.extend(providers.set_models(&name, models));
            }
            Ok(Err(error)) => {
                notices.push(Notice {
                    code: ErrorCode::ExtensionFailed,
                    extension: Some(extension.clone()),
                    message: format!("Could not refresh {name}'s model list: {error}"),
                });
            }
            Err(_) => {
                notices.push(Notice {
                    code: ErrorCode::ExtensionFailed,
                    extension: Some(extension.clone()),
                    message: format!(
                        "Could not refresh {name}'s model list: the refresh did not answer."
                    ),
                });
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
