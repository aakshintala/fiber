//! The providers installed extensions register as data, and the session's
//! model chosen from them (`docs/model-routing.md`, "Naming a model" and
//! "Choosing the model").

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use config::{Config, ModelData, ProviderData};
use contract::ErrorCode;
use contract::events::Notice;
use serde_json::Value;

use crate::{API, Error, LuaProvider};

/// The thinking levels a typed model may end in, after a `:`.
const THINKING: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Every provider the installed extensions register, by name.
#[derive(Clone, Default)]
pub struct Providers {
    by_name: BTreeMap<String, ProviderData>,
    /// Each provider's models' addendum texts, by provider name then
    /// model id: read with the data that declared them and replaced
    /// together with it.
    addenda: BTreeMap<String, BTreeMap<String, String>>,
    /// One Lua provider per `fiber.provider` registration, by provider
    /// name: what signs its requests and refreshes its token.
    lua: BTreeMap<String, Arc<LuaProvider>>,
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
    pub thinking: Option<&'static str>,
}

impl Model<'_> {
    /// The stored model reference, `provider/model`.
    pub fn reference(&self) -> String {
        format!("{}/{}", self.provider.name, self.model.id)
    }
}

/// Removes every model whose `web_search` its protocol does not read or
/// whose `extra_body` names a field Fiber builds, returning one
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

impl Providers {
    /// A typed model reference without any `:<level>` suffix, and the level
    /// the suffix named, if any: the single strip of a `:<level>` suffix both
    /// `resolve` and the `config set` model check use.
    pub fn split_thinking(typed: &str) -> (&str, Option<&'static str>) {
        typed
            .rsplit_once(':')
            .and_then(|(rest, level)| {
                THINKING
                    .iter()
                    .find(|known| **known == level)
                    .map(|known| (rest, Some(*known)))
            })
            .unwrap_or((typed, None))
    }

    /// Reads every extension in `extensions/` in Fiber home. One written for
    /// another extension API is left out, with a notice naming it and both
    /// numbers.
    pub fn load(home: &Path) -> Result<(Self, Vec<Notice>), Error> {
        let root = home.join("extensions");
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok((Self::default(), Vec::new())),
            Err(source) => return Err(Error::Io { path: root, source }),
        };
        let mut dirs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| Error::Io {
                path: root.clone(),
                source,
            })?;
            // A name starting with `.` is an install in progress.
            if !entry.file_name().to_string_lossy().starts_with('.') {
                dirs.push(entry.path());
            }
        }
        dirs.sort();
        let mut providers = Self::default();
        let mut notices = Vec::new();
        for dir in dirs {
            let manifest = config::read_manifest(&dir)?;
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
            let mut buffered: Vec<(ProviderData, BTreeMap<String, String>)> = Vec::new();
            let mut failed: Option<Notice> = None;
            for mut data in config::read_providers(&dir)? {
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
    /// extension. The provider is kept either way, for its signer and
    /// token.
    pub fn add_lua(
        &mut self,
        extension: &str,
        provider: &Arc<LuaProvider>,
        config: &Config,
    ) -> Vec<Notice> {
        let name = provider.name().to_owned();
        self.lua.insert(name.clone(), Arc::clone(provider));
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
                self.addenda.insert(name, addenda);
            }
            None => {
                self.addenda.insert(name.clone(), addenda);
                self.by_name.insert(
                    name.clone(),
                    ProviderData {
                        name,
                        models,
                        credential: None,
                        credential_name: None,
                        headers: BTreeMap::new(),
                        reviewer_model: None,
                    },
                );
            }
        }
        notices
    }

    /// The installed data of `name`: the data file's, else one naming only
    /// the provider, as [`LuaProvider::has_credential`] reads it.
    fn data(&self, name: &str) -> ProviderData {
        self.by_name.get(name).cloned().unwrap_or(ProviderData {
            name: name.to_owned(),
            models: Vec::new(),
            credential: None,
            credential_name: None,
            headers: BTreeMap::new(),
            reviewer_model: None,
        })
    }

    /// Drops every Lua provider except those in `keep`, by provider name:
    /// what unloads a refreshed provider the session does not use
    /// (`docs/model-routing.md`, "Model discovery"). The installed models
    /// stay: only the signer and token go.
    pub fn retain_lua(&mut self, keep: &[&str]) {
        self.lua.retain(|name, _| keep.contains(&name.as_str()));
    }

    /// The Lua provider `name`, for its signer and token, if an extension
    /// registered one.
    pub fn lua(&self, name: &str) -> Option<&Arc<LuaProvider>> {
        self.lua.get(name)
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
    /// exactly one installed provider has.
    pub fn resolve(&self, typed: &str) -> Result<Model<'_>, Error> {
        let (rest, thinking) = Self::split_thinking(typed);
        let mut tries = vec![(typed, None)];
        if thinking.is_some() {
            tries.push((rest, thinking));
        }
        for (text, thinking) in &tries {
            if let Some(found) = self.exact(text, *thinking) {
                return Ok(found);
            }
        }
        for (text, thinking) in &tries {
            let matches: Vec<Model<'_>> = self
                .by_name
                .values()
                .flat_map(|provider| {
                    provider
                        .models
                        .iter()
                        .filter(|model| model.id == *text)
                        .map(move |model| Model {
                            provider,
                            model,
                            thinking: *thinking,
                        })
                })
                .collect();
            match matches.as_slice() {
                [] => {}
                [one] => return Ok(*one),
                [..] => {
                    return Err(Error::Ambiguous {
                        id: (*text).into(),
                        matches: matches.iter().map(Model::reference).collect(),
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

    fn exact(&self, text: &str, thinking: Option<&'static str>) -> Option<Model<'_>> {
        let (name, id) = text.split_once('/')?;
        let provider = self.by_name.get(name)?;
        let model = provider.models.iter().find(|m| m.id == id)?;
        Some(Model {
            provider,
            model,
            thinking,
        })
    }
}

/// One started background refresh: the provider's name and its thread.
/// Joining is the caller's choice: a session never does, a one-shot child
/// does (`docs/model-routing.md`, "Model discovery").
pub type StartedRefresh = (String, JoinHandle<Result<Vec<ModelData>, crate::Error>>);

/// Refreshes one provider, or all providers that have a credential, with
/// or without the age check: every provider in `lua` with a credential
/// whose cached list `max_age` lets through (`None` runs whatever the
/// cache holds). One entry per provider started, in `lua` order. A refresh
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
