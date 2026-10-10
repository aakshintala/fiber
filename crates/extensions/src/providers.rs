//! The providers installed extensions register as data, and the session's
//! model chosen from them (`docs/model-routing.md`, "Naming a model" and
//! "Choosing the model").

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use config::{Config, ConfigError, ModelData, ProviderData};
use contract::ErrorCode;
use contract::events::Notice;
use serde_json::Value;

use crate::{API, Error, LuaProvider};
use contract::ThinkingLevel;

mod placeholders;
mod scripted;

/// Every provider the installed extensions register, by name.
#[derive(Clone, Default)]
pub struct Providers {
    by_name: BTreeMap<String, ProviderData>,
    /// Each model left out by [`Providers::fill_placeholders`], by
    /// provider name: its id and its `model_unconfigured` message, in
    /// model order. Never listed: only `resolve` reads it.
    unconfigured: BTreeMap<String, Vec<(String, String)>>,
    /// The extension whose data or `models()` supplied each provider's
    /// current models, by provider name.
    extension_of: BTreeMap<String, String>,
    /// Each provider's models' addendum texts, by provider name then
    /// model id: read with the data that declared them and replaced
    /// together with it.
    addenda: BTreeMap<String, BTreeMap<String, String>>,
    /// One Lua provider per `fiber.provider` registration, by provider
    /// name: what signs its requests and refreshes its token.
    lua: BTreeMap<String, Arc<LuaProvider>>,
    /// The secrets the installed extensions declare, each once
    /// (`docs/configuration.md`, "Secrets").
    secrets: BTreeSet<String>,
}

// `Arc<LuaProvider>` has no `Debug`: the registry prints its names.
impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Providers")
            .field("by_name", &self.by_name)
            .field("lua", &self.lua.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// One model of one installed provider, as a session names it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model<'a> {
    /// Its provider's data.
    pub provider: &'a ProviderData,
    /// The model's data.
    pub model: &'a ModelData,
    /// The thinking level a `:<level>` suffix asked for.
    pub thinking: Option<ThinkingLevel>,
}

impl Model<'_> {
    /// The stored model reference, `provider/model`.
    pub fn reference(&self) -> String {
        format!("{}/{}", self.provider.name, self.model.id)
    }
}

/// Removes every model that declares no `context_window`, whose `web_search`
/// its protocol does not read or whose `extra_body` names a field Fiber
/// builds, returning one
/// `model_invalid` notice per reason, in model order. A model it keeps is
/// unchanged.
pub fn leave_out_invalid(
    provider: &str,
    extension: &str,
    models: &mut Vec<ModelData>,
) -> Vec<Notice> {
    let mut notices = Vec::new();
    let mut kept = Vec::new();
    for model in models.drain(..) {
        let mut invalid = false;
        if let Some(kind) = model.web_search.as_deref()
            && !model.protocol.reads_web_search(kind)
        {
            notices.push(Notice {
                code: ErrorCode::ModelInvalid,
                message: format!(
                    "The model `{provider}/{}` names `{kind}` as its `web_search` type, \
                     which its protocol does not read.",
                    model.id
                ),
                extension: Some(extension.to_owned()),
            });
            invalid = true;
        }
        let reserved: Vec<&str> = model
            .protocol
            .reserved_body_fields()
            .iter()
            .filter(|field| model.extra_body.contains_key(**field))
            .copied()
            .collect();
        if !reserved.is_empty() {
            let fields = reserved
                .iter()
                .map(|field| format!("`{field}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let noun = if reserved.len() == 1 {
                "a field"
            } else {
                "fields"
            };
            let message = format!(
                "The model `{provider}/{}` names {fields} in its `extra_body`, \
                 {noun} Fiber builds itself.",
                model.id
            );
            notices.push(Notice {
                code: ErrorCode::ModelInvalid,
                message,
                extension: Some(extension.to_owned()),
            });
            invalid = true;
        }
        if let Some(default) = model.thinking_default
            && !model.thinking_levels.contains(&default)
        {
            notices.push(Notice {
                code: ErrorCode::ModelInvalid,
                message: format!(
                    "The model `{provider}/{}` names `{default}` as its `thinking_default`, \
                     which is not among its `thinking_levels`.",
                    model.id
                ),
                extension: Some(extension.to_owned()),
            });
            invalid = true;
        }
        if model.context_window.is_none_or(|window| window == 0) {
            notices.push(Notice {
                code: ErrorCode::ModelInvalid,
                message: format!(
                    "The model `{provider}/{}` declares no `context_window`.",
                    model.id
                ),
                extension: Some(extension.to_owned()),
            });
            invalid = true;
        }
        if !invalid {
            kept.push(model);
        }
    }
    *models = kept;
    notices
}

/// Reads every model's `prompt_addendum` file against `dir`, by model id.
/// A file that cannot be read is one `extension_failed` notice naming
/// `extension`, so `load` and `add_lua` share the one loop.
fn read_addenda(
    dir: &Path,
    models: &[ModelData],
    extension: &str,
) -> Result<BTreeMap<String, String>, Notice> {
    let mut addenda = BTreeMap::new();
    for model in models {
        if let Some(relative) = model.prompt_addendum.as_deref() {
            match config::read_package_text(dir, relative, "prompt_addendum") {
                Ok(text) => {
                    addenda.insert(model.id.clone(), text);
                }
                Err(e) => {
                    return Err(Notice {
                        code: ErrorCode::ExtensionFailed,
                        message: e.to_string(),
                        extension: Some(extension.to_owned()),
                    });
                }
            }
        }
    }
    Ok(addenda)
}

/// The notice leaving out a provider that claims the built-in provider's
/// name: `scripted` reaches no network (`docs/model-routing.md`, "The
/// scripted provider"), so no installed package may register it.
fn reserved_scripted(extension: &str) -> Notice {
    Notice {
        code: ErrorCode::ExtensionFailed,
        message: format!(
            "The provider `scripted` is reserved for the built-in scripted provider; \
             `{extension}`'s provider of that name is not loaded."
        ),
        extension: Some(extension.to_owned()),
    }
}

/// A model named by [`Providers::resolve`]: one it lists, or one left
/// out with `model_unconfigured`.
#[derive(Clone)]
enum Found<'a> {
    /// A configured model.
    Model(Model<'a>),
    /// A model [`Providers::fill_placeholders`] left out, with its
    /// reference and its notice's message.
    Unconfigured {
        /// `provider/model`.
        reference: String,
        /// Its `model_unconfigured` message.
        message: &'a str,
    },
}

impl<'a> Found<'a> {
    /// Its stored reference, `provider/model`.
    fn reference(&self) -> String {
        match self {
            Self::Model(model) => model.reference(),
            Self::Unconfigured { reference, .. } => reference.clone(),
        }
    }

    /// The session's model, or the `model_unconfigured` naming it.
    fn into_result(self) -> Result<Model<'a>, Error> {
        match self {
            Self::Model(model) => Ok(model),
            Self::Unconfigured { message, .. } => Err(Error::Unconfigured {
                message: message.to_owned(),
            }),
        }
    }
}

impl Providers {
    /// A typed model reference without any `:<level>` suffix, and the level
    /// the suffix named, if any: the single strip of a `:<level>` suffix both
    /// `resolve` and the `config set` model check use.
    pub fn split_thinking(typed: &str) -> (&str, Option<ThinkingLevel>) {
        typed
            .rsplit_once(':')
            .and_then(|(rest, level)| level.parse::<ThinkingLevel>().ok().map(|l| (rest, Some(l))))
            .unwrap_or((typed, None))
    }

    /// Reads every healthy extension in `extensions/` in Fiber home, in
    /// directory order. A damaged directory is skipped, as
    /// `docs/extensions.md`, "Installing", says: a directory with no
    /// `.fiber.json` record holds no installed extension. One written for
    /// another extension API is left out, with a notice naming it and both
    /// numbers.
    pub fn load(home: &Path) -> Result<(Self, Vec<Notice>), Error> {
        let entries = crate::installed::read_entries(home)?;
        let mut providers = Self::default();
        let mut notices = Vec::new();
        for entry in entries.found {
            let dir = entry.dir;
            let manifest = entry.manifest;
            if manifest.api != API {
                notices.push(Notice {
                    code: ErrorCode::ExtensionIncompatible,
                    message: Error::ApiVersion {
                        name: manifest.name.clone(),
                        api: manifest.api,
                    }
                    .to_string(),
                    extension: Some(manifest.name),
                });
                continue;
            }
            // Secrets do not depend on the provider files, so they are kept
            // even when those files leave the extension's providers out.
            providers.secrets.extend(manifest.secrets.iter().cloned());
            let mut buffered: Vec<(ProviderData, BTreeMap<String, String>)> = Vec::new();
            let mut failed: Option<Notice> = None;
            let mut datas = config::read_providers(&dir)?;
            // The built-in provider's name is reserved: a package that
            // claims it is left out, with one notice, like a bad addendum
            // leaves its providers out.
            datas.retain(|data| {
                if data.name != scripted::SCRIPTED {
                    return true;
                }
                notices.push(reserved_scripted(&manifest.name));
                false
            });
            for mut data in datas {
                // The cached `models()` list stands in for the data file's
                // until the refresh returns; a copy that is no list is no copy.
                if let Some(cached) = config::read_model_cache(home, &data.name)? {
                    data.models = cached;
                }
                notices.extend(leave_out_invalid(
                    &data.name,
                    &manifest.name,
                    &mut data.models,
                ));
                // One extension's addenda are read into a local buffer
                // against its own directory, and inserted only when every
                // one of its files succeeds: a bad addendum leaves out all
                // of its providers, with one notice, and other extensions
                // load (`docs/system-prompt.md`, "Extension texts").
                match read_addenda(&dir, &data.models, &manifest.name) {
                    Ok(addenda) => buffered.push((data, addenda)),
                    Err(notice) => {
                        failed = Some(notice);
                        break;
                    }
                }
            }
            if let Some(notice) = failed {
                notices.push(notice);
            } else {
                for (data, addenda) in buffered {
                    providers.addenda.insert(data.name.clone(), addenda);
                    providers
                        .extension_of
                        .insert(data.name.clone(), manifest.name.clone());
                    providers.by_name.insert(data.name.clone(), data);
                }
            }
        }
        Ok((providers, notices))
    }

    /// The installed providers' names, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    /// The secrets the installed extensions declare, sorted, each once.
    pub fn secrets(&self) -> impl Iterator<Item = &str> {
        self.secrets.iter().map(String::as_str)
    }

    /// Adds the models `provider`'s `models()` returns: with no data file
    /// one is created with every other field default, with one only its
    /// models are replaced. Either list passes through
    /// [`leave_out_invalid`]. With no cached copy `models()` runs
    /// synchronously, but only for a provider with a credential: with none
    /// it never runs, and the provider serves its data file's models, if
    /// any. A `models()` that fails leaves no models, or
    /// the data file's, and one notice with the error's own code. A model
    /// whose addendum file is missing or outside the package leaves no
    /// models from it, with one `extension_failed` notice naming the
    /// extension. The provider is kept either way, for its signer, token
    /// and cost lookup. A provider that did not register `models` only
    /// has its handle kept: no `models()` call and no notice. A provider
    /// named `scripted` is left out whole, handle and all, with one
    /// `extension_failed` notice naming `scripted` as reserved: the name
    /// belongs to the built-in provider.
    pub fn add_lua(
        &mut self,
        extension: &str,
        provider: &Arc<LuaProvider>,
        config: &Config,
    ) -> Vec<Notice> {
        let name = provider.name().to_owned();
        if name == scripted::SCRIPTED {
            return vec![reserved_scripted(extension)];
        }
        self.lua.insert(name.clone(), Arc::clone(provider));
        // A provider that registers no `models`, such as one with only
        // `cost()`, keeps its data file's models.
        if matches!(provider.registers("models"), Ok(false)) {
            return Vec::new();
        }
        if !provider.has_model_cache() && !provider.has_credential(config, &self.data(&name)) {
            return Vec::new();
        }
        let mut models = match provider.models() {
            Ok(models) => models,
            Err(e) => {
                return vec![Notice {
                    code: e.code(),
                    message: e.to_string(),
                    extension: Some(extension.to_owned()),
                }];
            }
        };
        let notices = leave_out_invalid(&name, extension, &mut models);
        // The addenda resolve against the Lua extension's package
        // directory, replacing the data file's with the final models.
        let addenda = match read_addenda(provider.dir(), &models, extension) {
            Ok(addenda) => addenda,
            Err(notice) => return vec![notice],
        };
        match self.by_name.get_mut(&name) {
            Some(data) => {
                data.models = models;
                self.addenda.insert(name.clone(), addenda);
                self.extension_of.insert(name, extension.to_owned());
            }
            None => {
                self.addenda.insert(name.clone(), addenda);
                self.extension_of.insert(name.clone(), extension.to_owned());
                self.by_name.insert(
                    name.clone(),
                    ProviderData {
                        name,
                        models,
                        credential: None,
                        credential_name: None,
                        headers: BTreeMap::new(),
                        placeholders: BTreeMap::new(),
                        reviewer_model: None,
                        login: None,
                    },
                );
            }
        }
        notices
    }

    /// Fills every `{name}` in every model's `base_url` from the provider's
    /// extension's setting `name`, never the repository's file, else the
    /// environment variable `placeholders.<name>.env` names, read through
    /// `env` (`docs/model-routing.md`, "A per-account host"). A value fills
    /// only when it is a host; a model with a placeholder that has no value
    /// or a value that is not a host is removed, with one `model_unconfigured`
    /// notice. Runs once, after `load` and every `add_lua`, before a model is
    /// chosen or listed; the model cache keeps the template. An extension
    /// settings file that cannot be read is `Error::Config`.
    pub fn fill_placeholders(
        &mut self,
        config: &Config,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Vec<Notice>, Error> {
        let mut notices = Vec::new();
        let names: Vec<String> = self.by_name.keys().cloned().collect();
        for provider_name in names {
            let extension = self
                .extension_of
                .get(&provider_name)
                .cloned()
                .unwrap_or_else(|| provider_name.clone());
            let placeholders = self
                .by_name
                .get(&provider_name)
                .map(|data| data.placeholders.clone())
                .unwrap_or_default();
            let mut left_out = Vec::new();
            if let Some(data) = self.by_name.get_mut(&provider_name) {
                let mut kept = Vec::new();
                for mut model in data.models.drain(..) {
                    let template = model.base_url.clone();
                    let lookup = |name: &str| -> Result<
                        Option<(String, placeholders::Source)>,
                        ConfigError,
                    > {
                        match config.extensions().get(&extension, &[], name)? {
                            Some(Value::String(value)) if !value.is_empty() => {
                                Ok(Some((value, placeholders::Source::Setting)))
                            }
                            // Absent, JSON null, or an empty string leaves the
                            // environment fallback in play. Any other present
                            // value is the setting's: it is never a host, so it
                            // fills as `Source::Setting` without consulting the
                            // environment. The stand-in never reaches a URL or a
                            // notice: `fill` reports it as not-a-host, and that
                            // notice never repeats the value.
                            Some(Value::String(_)) | Some(Value::Null) | None => {
                                if let Some(variable) = placeholders
                                    .get(name)
                                    .and_then(|placeholder| placeholder.env.as_deref())
                                    && let Some(value) = env(variable)
                                    && !value.is_empty()
                                {
                                    return Ok(Some((
                                        value,
                                        placeholders::Source::Env(variable.to_owned()),
                                    )));
                                }
                                Ok(None)
                            }
                            Some(_) => Ok(Some((" ".to_owned(), placeholders::Source::Setting))),
                        }
                    };
                    match placeholders::fill(&template, &lookup)? {
                        placeholders::Filled::Url(url) => {
                            model.base_url = url;
                            kept.push(model);
                        }
                        placeholders::Filled::Missing(name) => {
                            let variable = placeholders
                                .get(&name)
                                .and_then(|placeholder| placeholder.env.as_deref());
                            let notice = placeholders::unconfigured(
                                &provider_name,
                                &model.id,
                                &extension,
                                &name,
                                variable,
                            );
                            left_out.push((model.id.clone(), notice.message.clone()));
                            notices.push(notice);
                        }
                        placeholders::Filled::NotHost { name, source } => {
                            let notice = placeholders::not_a_host(
                                &provider_name,
                                &model.id,
                                &extension,
                                &name,
                                &source,
                            );
                            left_out.push((model.id.clone(), notice.message.clone()));
                            notices.push(notice);
                        }
                    }
                }
                data.models = kept;
            }
            if !left_out.is_empty() {
                self.unconfigured.insert(provider_name, left_out);
            }
        }
        Ok(notices)
    }

    /// The extension that registered `name`, if a Lua extension did.
    pub fn extension_of(&self, name: &str) -> Option<&str> {
        self.extension_of.get(name).map(String::as_str)
    }

    /// The installed data of `name`: the data file's, else one naming only
    /// the provider, as [`LuaProvider::has_credential`] reads it.
    pub fn data(&self, name: &str) -> ProviderData {
        self.by_name.get(name).cloned().unwrap_or(ProviderData {
            name: name.to_owned(),
            models: Vec::new(),
            credential: None,
            credential_name: None,
            headers: BTreeMap::new(),
            placeholders: BTreeMap::new(),
            reviewer_model: None,
            login: None,
        })
    }

    /// Adds the cached model list of `name`, with the installed data file's
    /// other fields: only when no provider is held under `name`, and never
    /// for `scripted`, which no installed package may register. Whether it
    /// inserted: `false` leaves the registry as it was.
    pub fn add_cached(&mut self, name: &str, models: Vec<ModelData>) -> bool {
        if name == scripted::SCRIPTED || self.by_name.contains_key(name) {
            return false;
        }
        let mut data = self.data(name);
        data.models = models;
        self.by_name.insert(name.to_owned(), data);
        true
    }

    /// Replaces the models held under `name` with a background refresh's
    /// joined list, inserting with default other fields when none is held:
    /// the refresh already wrote the cache file. Never for `scripted`,
    /// which no installed package may register.
    pub fn set_models(&mut self, name: &str, models: Vec<ModelData>) {
        if name == scripted::SCRIPTED {
            return;
        }
        match self.by_name.get_mut(name) {
            Some(data) => data.models = models,
            None => {
                let mut data = self.data(name);
                data.models = models;
                self.by_name.insert(name.to_owned(), data);
            }
        }
    }

    /// The Lua provider `name`, for its signer and token, if an extension
    /// registered one.
    pub fn lua(&self, name: &str) -> Option<&Arc<LuaProvider>> {
        self.lua.get(name)
    }

    /// Drops every Lua provider handle and keeps the models, placeholders
    /// and addenda they supplied: a registry that names every installed
    /// model and holds no extension's VM.
    pub fn forget_lua(&mut self) {
        self.lua.clear();
    }

    /// The text of the chosen model's addendum file; `None` when the model
    /// names none.
    pub fn addendum(&self, model: &Model<'_>) -> Option<&str> {
        self.addenda
            .get(model.provider.name.as_str())?
            .get(model.model.id.as_str())
            .map(String::as_str)
    }

    /// The provider installed under `name`.
    pub fn get(&self, name: &str) -> Option<&ProviderData> {
        self.by_name.get(name)
    }

    /// The session's model, in the order "Choosing the model" gives: the
    /// model a resumed session was using, then configuration's `model`, which
    /// `--model` and `-c model=` set for one run.
    pub fn choose<'a>(
        &'a self,
        resumed: Option<&str>,
        config: &Config,
    ) -> Result<Model<'a>, Error> {
        let configured = config.get("model", None).map(|(value, _)| value);
        let typed = resumed
            .or_else(|| configured.as_ref().and_then(Value::as_str))
            .ok_or(Error::NoModel)?;
        self.resolve(typed)
    }

    /// A model as a person types it: the exact `provider/model`, then the
    /// same with a `:<thinking level>` suffix taken off, then a bare id that
    /// exactly one installed provider has. The configured and unconfigured
    /// models are matched together: naming one left out with
    /// `model_unconfigured` is that error, not `no_model`, and a bare id
    /// two providers share is `model_ambiguous` whatever mix they are.
    pub fn resolve(&self, typed: &str) -> Result<Model<'_>, Error> {
        let (rest, thinking) = Self::split_thinking(typed);
        let mut tries = vec![(typed, None)];
        if thinking.is_some() {
            tries.push((rest, thinking));
        }
        for (text, thinking) in &tries {
            if let Some(found) = self.exact(text, *thinking) {
                return found.into_result();
            }
        }
        for (text, thinking) in &tries {
            let matches: Vec<Found<'_>> =
                self.by_name
                    .values()
                    .filter(|provider| provider.name != scripted::SCRIPTED)
                    .flat_map(|provider| {
                        let configured = provider
                            .models
                            .iter()
                            .filter(|model| model.id == *text)
                            .map(move |model| {
                                Found::Model(Model {
                                    provider,
                                    model,
                                    thinking: *thinking,
                                })
                            });
                        let unconfigured =
                            self.unconfigured
                                .get(provider.name.as_str())
                                .map(|left_out| {
                                    left_out.iter().filter(|(id, _)| id == text).map(
                                        |(id, message)| Found::Unconfigured {
                                            reference: format!("{}/{id}", provider.name),
                                            message,
                                        },
                                    )
                                })
                                .into_iter()
                                .flatten();
                        configured.chain(unconfigured)
                    })
                    .collect();
            match matches.as_slice() {
                [] => {}
                [one] => return one.clone().into_result(),
                [..] => {
                    return Err(Error::Ambiguous {
                        id: (*text).into(),
                        matches: matches.iter().map(Found::reference).collect(),
                    });
                }
            }
        }
        let text = rest;
        Err(match text.split_once('/') {
            Some((provider, model)) if self.by_name.contains_key(provider) => Error::UnknownModel {
                provider: provider.into(),
                model: model.into(),
            },
            Some((provider, _)) => Error::ProviderMissing {
                provider: provider.into(),
            },
            None => Error::ModelMissing { id: text.into() },
        })
    }

    /// The exact `provider/model` in `text`: its configured model first,
    /// then its model left out with `model_unconfigured`.
    fn exact(&self, text: &str, thinking: Option<ThinkingLevel>) -> Option<Found<'_>> {
        let (name, id) = text.split_once('/')?;
        let provider = self.by_name.get(name)?;
        if let Some(model) = provider.models.iter().find(|m| m.id == id) {
            return Some(Found::Model(Model {
                provider,
                model,
                thinking,
            }));
        }
        let message = self
            .unconfigured
            .get(name)?
            .iter()
            .find(|(left, _)| left == id)?
            .1
            .as_str();
        Some(Found::Unconfigured {
            reference: format!("{name}/{id}"),
            message,
        })
    }
}

/// One started background refresh: the provider's name and its thread.
/// Joining is the caller's choice: a session never does, a one-shot child
/// does (`docs/model-routing.md`, "Model discovery").
pub type StartedRefresh = (String, JoinHandle<Result<Vec<ModelData>, crate::Error>>);

/// Refreshes one provider, or all providers that have a credential, with
/// or without the age check: every provider in `lua` that registered
/// `models` and has a credential, whose cached list `max_age` lets through
/// (`None` runs whatever the cache holds). One entry per provider started,
/// in `lua` order. A refresh
/// writes only the cache file and the provider's in-memory list, never this
/// `Providers`: a running session's tool definitions never change, and
/// nothing here schedules a later refresh (`docs/model-routing.md`, "Model
/// discovery").
pub fn refresh_lists(
    lua: &[Arc<LuaProvider>],
    providers: &Providers,
    config: &Config,
    max_age: Option<Duration>,
) -> Vec<StartedRefresh> {
    let mut started = Vec::new();
    for provider in lua {
        if !provider.has_credential(config, &providers.data(provider.name())) {
            continue;
        }
        if let Some(handle) = provider.refresh(max_age) {
            started.push((provider.name().to_owned(), handle));
        }
    }
    started
}
