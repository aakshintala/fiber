//! The providers installed extensions register as data, and the session's
//! model chosen from them (`docs/model-routing.md`, "Naming a model" and
//! "Choosing the model").

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use config::{Config, ModelData, ProviderData, Source};
use contract::ErrorCode;
use contract::events::Notice;

use crate::git::short_name;
use crate::{API, Error, LuaProvider, ModelsArg, SessionExtensions};
use contract::ThinkingLevel;

mod declared;
mod placeholders;
mod scripted;

pub(crate) use declared::{first_undeclared, provider_scripts};

/// Every provider the installed extensions register, by name.
#[derive(Clone, Default)]
pub struct Providers {
    by_name: BTreeMap<String, ProviderData>,
    /// Each model left out by [`Providers::fill_placeholders`], by
    /// provider name: its id and its `model_unconfigured` message, in
    /// model order. Never listed: only `resolve` reads it.
    unconfigured: BTreeMap<String, Vec<(String, String)>>,
    /// Each loaded extension's manifest `providers` map, by extension
    /// name: what its registrations are admitted against.
    declared: BTreeMap<String, BTreeMap<String, Vec<String>>>,
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
    /// The secrets the installed extensions declare, by extension:
    /// two extensions sharing a secret each keep their row here, while
    /// [`Providers::secrets`] still lists the name once
    /// (`docs/configuration.md`, "Secrets").
    secrets_by_extension: BTreeMap<String, BTreeSet<String>>,
    /// The loaded extensions that need code to serve their providers: a
    /// `process` program, an `init.lua`, or a `providers/*.lua` script.
    /// Doctor checks only providers whose extension is not in here.
    coded: BTreeSet<String>,
    /// The providers an enabled, healthy, compatible extension declared
    /// with Lua code that could register, by provider name: an `init.lua`
    /// can register any of its extension's declared providers, and a
    /// `providers/<name>.lua` script its own. Recorded during the load
    /// without running code; a process-only package supplies no Lua, and
    /// disabled, damaged or incompatible extensions record nothing.
    code_candidates: BTreeSet<String>,
    /// Each loaded candidate's directory, by extension name: the pinned
    /// copy for a repository entry, the personal install otherwise. What
    /// [`Providers::provider_source_dir`] resolves a login to.
    source_dirs: BTreeMap<String, std::path::PathBuf>,
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
/// Callers reject a staged package or leave an installed package out with
/// an `extension_failed` notice when a file cannot be read.
fn read_addenda(
    dir: &Path,
    models: &[ModelData],
) -> Result<BTreeMap<String, String>, config::ConfigError> {
    let mut addenda = BTreeMap::new();
    for model in models {
        if let Some(relative) = model.prompt_addendum.as_deref() {
            let text = config::read_package_text(dir, relative, "prompt_addendum")?;
            addenda.insert(model.id.clone(), text);
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
    /// numbers. Each damaged directory leaves one `extension_failed`
    /// notice naming it and the fix, before the healthy entries' notices.
    pub fn load(home: &Path) -> Result<(Self, Vec<Notice>), Error> {
        Self::load_with(home, |_: &str| true, &crate::RepositorySet::default())
    }

    /// Reads the installed providers enabled by the effective configuration.
    /// Disabled packages contribute no providers, declarations or secrets.
    pub fn load_enabled(
        home: &Path,
        config: &config::Config,
    ) -> Result<(Self, Vec<Notice>), Error> {
        Self::load_with(
            home,
            |name: &str| crate::is_enabled(config, name),
            &crate::RepositorySet::default(),
        )
    }

    /// Reads the session's providers from the merged personal and
    /// repository entries: an approved repository copy replaces the
    /// personal install of the same name, with one notice naming both,
    /// whether or not it is enabled. A damaged personal install keeps
    /// its `extension_failed` notice.
    pub fn load_repository(
        home: &Path,
        config: &config::Config,
        repository: &crate::RepositorySet,
    ) -> Result<(Self, Vec<Notice>), Error> {
        Self::load_with(
            home,
            |name: &str| crate::is_enabled(config, name),
            repository,
        )
    }

    /// Reads every installed provider merged with the repository set,
    /// with no enabled filter: every personal install loads, as
    /// [`Providers::load`] reads, with approved repository entries merged
    /// in. An empty set loads what `load` loads.
    pub fn load_all_repository(
        home: &Path,
        repository: &crate::RepositorySet,
    ) -> Result<(Self, Vec<Notice>), Error> {
        Self::load_with(home, |_: &str| true, repository)
    }

    /// Reads the provider table an install will put in place before committing.
    pub fn load_planned(home: &Path, plan: &crate::Plan<'_>) -> Result<(Self, Vec<Notice>), Error> {
        let mut entries = crate::installed::read_entries(home)?;
        let notices = entries
            .damaged
            .iter()
            .map(|hit| Notice {
                code: ErrorCode::ExtensionFailed,
                message: hit.to_string(),
                extension: Some(short_name(&hit.name).to_owned()),
            })
            .collect();
        entries
            .found
            .retain(|entry| !plan.items().any(|item| item.name == entry.installed.name));
        let (installed, _) =
            crate::repository::candidates(entries.found, &crate::RepositorySet::default(), &|_| {
                true
            });
        let mut candidates: Vec<_> = installed
            .into_iter()
            .map(|candidate| (candidate, false))
            .collect();
        for item in plan.items() {
            candidates.push((
                crate::repository::Candidate {
                    name: item.name.clone(),
                    version: item.version.clone(),
                    dir: item.staged().to_path_buf(),
                    manifest: item.manifest.clone(),
                    key: crate::install::slug(&item.name)?,
                },
                true,
            ));
        }
        candidates.sort_by(|(a, _), (b, _)| a.key.cmp(&b.key));
        Self::load_candidates(home, candidates, notices)
    }

    fn load_with(
        home: &Path,
        enabled: impl Fn(&str) -> bool,
        repository: &crate::RepositorySet,
    ) -> Result<(Self, Vec<Notice>), Error> {
        let entries = crate::installed::read_entries(home)?;
        let mut notices = Vec::new();
        // A damaged directory contributes no provider, model or secret:
        // one notice naming it and the fix, in the order `read_entries`
        // sorts them.
        for hit in &entries.damaged {
            notices.push(Notice {
                code: ErrorCode::ExtensionFailed,
                message: hit.to_string(),
                extension: Some(short_name(&hit.name).to_owned()),
            });
        }
        // An approved repository copy replaces the personal install of
        // the same name: the merged list carries both in slug order, with
        // one notice per shadowed install.
        let (merged, mut shadows) =
            crate::repository::candidates(entries.found, repository, &enabled);
        notices.append(&mut shadows);
        Self::load_candidates(
            home,
            merged.into_iter().map(|candidate| (candidate, false)),
            notices,
        )
    }

    fn load_candidates(
        home: &Path,
        candidates: impl IntoIterator<Item = (crate::repository::Candidate, bool)>,
        mut notices: Vec<Notice>,
    ) -> Result<(Self, Vec<Notice>), Error> {
        let mut providers = Self::default();
        for (candidate, staged) in candidates {
            let dir = candidate.dir;
            let manifest = candidate.manifest;
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
            // The candidate's directory is what a login resolves the
            // extension to: the pinned copy for a repository entry.
            providers
                .source_dirs
                .insert(manifest.name.clone(), dir.clone());
            providers
                .secrets_by_extension
                .entry(manifest.name.clone())
                .or_default()
                .extend(manifest.secrets.iter().cloned());
            // A provider served with code in the loop cannot be checked
            // without running it: a `process` program, an `init.lua`, or
            // a `providers/*.lua` script names the extension coded.
            if manifest.process.is_some()
                || dir.join("init.lua").is_file()
                || provider_scripts(&dir).is_ok_and(|scripts| !scripts.is_empty())
            {
                providers.coded.insert(manifest.name.clone());
            }
            let mut datas = config::read_providers(&dir)?;
            // A provider the manifest does not declare under `providers`
            // is not loaded: none of the extension's providers is, with
            // one notice naming the first undeclared one. This report is
            // the only one; the session refuses the same extension
            // silently, since it cannot run Lua here.
            {
                let names: Vec<&str> = datas.iter().map(|data| data.name.as_str()).collect();
                if let Some(unclaimed) = declared::first_undeclared(&manifest.providers, names) {
                    notices.push(Notice {
                        code: ErrorCode::ExtensionFailed,
                        message: Error::UndeclaredProvider {
                            extension: manifest.name.clone(),
                            provider: unclaimed.to_owned(),
                        }
                        .to_string(),
                        extension: Some(manifest.name),
                    });
                    continue;
                }
            }
            providers
                .declared
                .insert(manifest.name.clone(), manifest.providers.clone());
            providers.record_code_candidates(&dir, &manifest);
            let mut buffered: Vec<(ProviderData, BTreeMap<String, String>)> = Vec::new();
            let mut failed: Option<Notice> = None;
            // The built-in provider's name is reserved: a package that
            // claims it keeps none of its providers, with one
            // `extension_failed` notice naming the package.
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
                notices.extend(providers.admit(&data.name, &manifest.name, &mut data.models));
                // One extension's addenda are read into a local buffer
                // against its own directory, and inserted only when every
                // one of its files succeeds: a bad addendum leaves out all
                // of its providers, with one notice, and other extensions
                // load (`docs/system-prompt.md`, "Extension texts").
                match read_addenda(&dir, &data.models) {
                    Ok(addenda) => buffered.push((data, addenda)),
                    Err(error) => {
                        if staged {
                            return Err(Error::Config(error));
                        }
                        failed = Some(Notice {
                            code: ErrorCode::ExtensionFailed,
                            message: error.to_string(),
                            extension: Some(manifest.name.clone()),
                        });
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

    /// Records the declared providers Lua code could register: every
    /// declared provider when an `init.lua` is present — the
    /// conservative criterion, an uncertain registration is never a
    /// definite absence — else only the providers with their own
    /// `providers/<name>.lua` script. A process-only package supplies
    /// no Lua. Runs during the load, without running code.
    fn record_code_candidates(&mut self, dir: &Path, manifest: &config::Manifest) {
        let init = dir.join("init.lua").is_file();
        let scripts = provider_scripts(dir).unwrap_or_default();
        if !init && scripts.is_empty() {
            return;
        }
        if let Some(declared) = self.declared.get(&manifest.name) {
            for provider in declared.keys() {
                if init || scripts.iter().any(|script| script == provider) {
                    self.code_candidates.insert(provider.clone());
                }
            }
        }
    }

    /// Leaves out the models `extension` may not serve: first the invalid
    /// ones, then the ones whose `base_url` the manifest's entry for
    /// `provider` does not list. A missing entry admits no model. Every
    /// extension-supplied model list passes through here before entering
    /// the registry; only the built-in scripted provider's model does not.
    fn admit(&self, provider: &str, extension: &str, models: &mut Vec<ModelData>) -> Vec<Notice> {
        let mut notices = leave_out_invalid(provider, extension, models);
        let empty = Vec::new();
        let listed = self
            .declared
            .get(extension)
            .and_then(|entries| entries.get(provider))
            .unwrap_or(&empty);
        notices.extend(declared::leave_out_unlisted(
            provider, extension, listed, models,
        ));
        notices
    }

    /// Drops every provider the session refused for an undeclared Lua
    /// registration, along with its addenda and declarations, then adds
    /// each Lua provider's models. The one call every path that runs Lua
    /// makes after `load`, so no caller can forget the drop. It gives no
    /// notice of its own: the rejection's notice is the session's.
    pub fn add_session(
        &mut self,
        extensions: &SessionExtensions,
        config: &Config,
        run: config::Runner<'_>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Notice> {
        for name in extensions.rejected() {
            let dropped: Vec<String> = self
                .extension_of
                .iter()
                .filter(|(_, owner)| *owner == name)
                .map(|(provider, _)| provider.clone())
                .collect();
            for provider in dropped {
                self.by_name.remove(&provider);
                self.extension_of.remove(&provider);
                self.addenda.remove(&provider);
                self.unconfigured.remove(&provider);
            }
            self.declared.remove(name);
        }
        let mut notices = Vec::new();
        for (extension, provider) in extensions.lua_providers() {
            notices.extend(self.add_lua(extension, provider, config, run, env));
        }
        notices
    }

    /// The installed providers' names, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    /// Every installed extension's declared secret names, by extension
    /// then name: two extensions sharing a secret each keep their row,
    /// while [`Providers::secrets`] still lists the name once.
    /// Incompatible and damaged packages contribute none, and a
    /// provider-admission failure keeps the extension's secrets, which
    /// do not depend on its providers.
    pub fn declared_secrets(&self) -> impl Iterator<Item = (&str, &str)> {
        self.secrets_by_extension
            .iter()
            .flat_map(|(extension, secrets)| {
                secrets
                    .iter()
                    .map(|secret| (extension.as_str(), secret.as_str()))
            })
    }

    /// Whether the provider can be checked without running extension
    /// code. A provider in data is checkable when its extension serves
    /// it with no `process` program, `init.lua` or `providers/*.lua`
    /// script. A provider absent from data is uncertain, never checked:
    /// it could register when an enabled, healthy, compatible extension
    /// declared it with Lua code that could register it. A disabled,
    /// damaged or incompatible extension's providers are absent, and a
    /// Lua-registered provider needs its code, so neither registers.
    pub fn could_register(&self, provider: &str) -> bool {
        if let Some(owner) = self.extension_of.get(provider) {
            return !self.coded.contains(owner);
        }
        self.code_candidates.contains(provider)
    }

    /// The secrets the installed extensions declare, sorted, each once.
    pub fn secrets(&self) -> impl Iterator<Item = &str> {
        self.secrets_by_extension
            .values()
            .flatten()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            .into_iter()
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
        run: config::Runner<'_>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Notice> {
        let name = provider.name().to_owned();
        if name == scripted::SCRIPTED {
            // An undeclared `scripted` fails as undeclared; a declared one
            // keeps the reserved-name notice.
            let undeclared = self
                .declared
                .get(extension)
                .is_none_or(|entries| !entries.contains_key(&name));
            if undeclared {
                let error = Error::UndeclaredProvider {
                    extension: extension.to_owned(),
                    provider: name.clone(),
                };
                return vec![Notice {
                    code: ErrorCode::ExtensionFailed,
                    message: error.to_string(),
                    extension: Some(extension.to_owned()),
                }];
            }
            return vec![reserved_scripted(extension)];
        }
        self.lua.insert(name.clone(), Arc::clone(provider));
        // Registering through Lua is code in the loop: the provider
        // cannot be checked without running it.
        self.coded.insert(extension.to_owned());
        // A provider that registers no `models`, such as one with only
        // `cost()`, keeps its data file's models. With a cached copy the
        // cache serves and `models()` never runs, so the argument stays
        // empty.
        let arg = if provider.has_model_cache() {
            ModelsArg::default()
        } else {
            if matches!(provider.registers("models"), Ok(false)) {
                return Vec::new();
            }
            if !provider.has_credential(config, &self.data(&name), run) {
                return Vec::new();
            }
            match self.models_arg(extension, provider, config, run, env) {
                Ok(Some(arg)) => arg,
                // A declared placeholder has no host: `models()` is not
                // called, and the provider serves its data file's
                // models, which `fill_placeholders` leaves out with
                // `model_unconfigured` (`docs/model-routing.md`, "A
                // per-account host").
                Ok(None) => return Vec::new(),
                Err(error) => {
                    return vec![Notice {
                        code: error.code(),
                        message: error.to_string(),
                        extension: Some(extension.to_owned()),
                    }];
                }
            }
        };
        let mut models = match provider.models(&arg) {
            Ok(models) => models,
            Err(e) => {
                return vec![Notice {
                    code: e.code(),
                    message: e.to_string(),
                    extension: Some(extension.to_owned()),
                }];
            }
        };
        let notices = self.admit(&name, extension, &mut models);
        // The addenda resolve against the Lua extension's package
        // directory, replacing the data file's with the final models.
        let addenda = match read_addenda(provider.dir(), &models) {
            Ok(addenda) => addenda,
            Err(error) => {
                return vec![Notice {
                    code: ErrorCode::ExtensionFailed,
                    message: error.to_string(),
                    extension: Some(extension.to_owned()),
                }];
            }
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

    /// The argument `provider`'s `models()` receives: the session's key,
    /// absent when the provider registers `credential()`, beside the host
    /// each declared placeholder holds (`docs/model-routing.md`, "Model
    /// discovery"). `None` when a declared placeholder has no host:
    /// `models()` is not called, and with no cached copy the data file's
    /// models serve until `fill_placeholders` leaves them out with
    /// `model_unconfigured`. A credential source or settings file that
    /// cannot be read is `Error::Config`, never an absent key.
    fn models_arg(
        &self,
        extension: &str,
        provider: &LuaProvider,
        config: &Config,
        run: config::Runner<'_>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<ModelsArg>, Error> {
        let data = self.data(provider.name());
        let key = if provider.registers("credential").unwrap_or(false) {
            None
        } else {
            let read = config.credentials().credential_with(
                &data,
                &config.credentials().credential_label(&data),
                run,
            )?;
            Some(read.secret)
        };
        let declared = data.placeholders;
        let lookup = |name: &str| placeholders::value(config, extension, &declared, name, env);
        let Some(hosts) = placeholders::hosts(&declared, &lookup)? else {
            return Ok(None);
        };
        Ok(Some(ModelsArg {
            key,
            placeholders: hosts,
        }))
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
                    let lookup = |name: &str| {
                        placeholders::value(config, &extension, &placeholders, name, env)
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

    /// Each declared placeholder of provider `name` with no present
    /// value: its name with its declared environment variable, in
    /// declaration order. A gap exists exactly when
    /// [`placeholders::value`] returns nothing — no setting string and
    /// no environment-variable string — regardless of validity, so a
    /// present-but-invalid value is no gap. Read through the same lookup
    /// [`Providers::fill_placeholders`] uses.
    pub fn unset_placeholders(
        &self,
        config: &Config,
        env: &dyn Fn(&str) -> Option<String>,
        name: &str,
    ) -> Result<Vec<(String, Option<String>)>, Error> {
        let data = self.data(name);
        let extension = self.extension_of(name).unwrap_or(name);
        let mut gaps = Vec::new();
        for (key, declared) in &data.placeholders {
            if placeholders::value(config, extension, &data.placeholders, key, env)?.is_none() {
                gaps.push((key.clone(), declared.env.clone()));
            }
        }
        Ok(gaps)
    }

    /// `value` stripped as [`placeholders::host`] strips it: at most one
    /// leading `https://` and one trailing `/`, then a host with an
    /// optional port. The prompt answers are validated only through here,
    /// never by widening `placeholders::host`, so the prompt and the model
    /// list can never disagree.
    pub fn validate_host(value: &str) -> Option<&str> {
        placeholders::host(value)
    }

    /// The extension that registered `name`, if a Lua extension did.
    pub fn extension_of(&self, name: &str) -> Option<&str> {
        self.extension_of.get(name).map(String::as_str)
    }

    /// The directory of the loaded candidate that registered `provider`:
    /// the pinned copy for a repository entry, the personal install
    /// otherwise. `None` when the provider's extension never loaded as a
    /// candidate, so the caller falls back to the home layout.
    pub fn provider_source_dir(&self, provider: &str) -> Option<std::path::PathBuf> {
        self.extension_of(provider)
            .and_then(|extension| self.source_dirs.get(extension))
            .cloned()
    }

    /// The loaded candidate's directory for the full extension name:
    /// the pinned copy for a repository entry, the personal install
    /// otherwise. `None` when the extension never loaded as a candidate,
    /// so the caller falls back to the home layout. Only directories the
    /// load already admitted, never a repository's working files.
    pub fn extension_source_dir(&self, extension: &str) -> Option<std::path::PathBuf> {
        self.source_dirs.get(extension).cloned()
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
    /// for `scripted`, which no installed package may register. The
    /// extension checked against is the first in name order whose manifest
    /// declares `name`. Whether it inserted, with the admission notices:
    /// `None` leaves the registry as it was: `scripted`, already held, or
    /// no installed extension declares `name`.
    pub fn add_cached(&mut self, name: &str, models: Vec<ModelData>) -> Option<Vec<Notice>> {
        if name == scripted::SCRIPTED || self.by_name.contains_key(name) {
            return None;
        }
        let owner = self
            .declared
            .iter()
            .find(|(_, entries)| entries.contains_key(name))
            .map(|(extension, _)| extension.clone())?;
        let mut models = models;
        let notices = self.admit(name, &owner, &mut models);
        let mut data = self.data(name);
        data.models = models;
        self.by_name.insert(name.to_owned(), data);
        self.extension_of.insert(name.to_owned(), owner);
        Some(notices)
    }

    /// Replaces the models held under `name` with a background refresh's
    /// joined list, inserting with default other fields when none is held:
    /// the refresh already wrote the cache file. Never for `scripted`,
    /// which no installed package may register. The extension checked
    /// against is the first in name order whose manifest declares `name`;
    /// with none, every model is left out, `name` standing in as the
    /// extension in the notice, and no owner is recorded.
    pub fn set_models(&mut self, name: &str, models: Vec<ModelData>) -> Vec<Notice> {
        if name == scripted::SCRIPTED {
            return Vec::new();
        }
        let owner = self
            .declared
            .iter()
            .find(|(_, entries)| entries.contains_key(name))
            .map(|(extension, _)| extension.clone());
        let mut models = models;
        let Some(owner) = owner else {
            let mut notices = leave_out_invalid(name, name, &mut models);
            notices.extend(declared::leave_out_unlisted(name, name, &[], &mut models));
            return notices;
        };
        let notices = self.admit(name, &owner, &mut models);
        match self.by_name.get_mut(name) {
            Some(data) => data.models = models,
            None => {
                let mut data = self.data(name);
                data.models = models;
                self.by_name.insert(name.to_owned(), data);
            }
        }
        self.extension_of.insert(name.to_owned(), owner);
        notices
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
    /// per-run layer first (`--model` via `per_run`, then `-c model=`),
    /// then the model a resumed session was using, then configuration's
    /// default.
    pub fn choose<'a>(
        &'a self,
        resumed: Option<&str>,
        config: &Config,
    ) -> Result<Model<'a>, Error> {
        let configured = config.get("model", None);
        // Only the per-run layer beats the recorded model: a persistent
        // default never does (`docs/model-routing.md`, "Choosing the
        // model").
        let per_run = configured
            .as_ref()
            .filter(|(_, source)| *source == Source::Run)
            .and_then(|(value, _)| value.as_str());
        let fallback = configured.as_ref().and_then(|(value, _)| value.as_str());
        let typed = per_run.or(resumed).or(fallback).ok_or(Error::NoModel)?;
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
    run: config::Runner<'_>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<StartedRefresh> {
    let mut started = Vec::new();
    for provider in lua {
        if provider.fresh(max_age) {
            continue;
        }
        if !provider.has_credential(config, &providers.data(provider.name()), run) {
            continue;
        }
        // A declared placeholder with no host starts nothing: the same
        // read would fail `fill_placeholders`, which every caller
        // already reports (`docs/model-routing.md`, "A per-account
        // host").
        let extension = providers
            .extension_of(provider.name())
            .unwrap_or(provider.name());
        let arg = match providers.models_arg(extension, provider, config, run, env) {
            Ok(Some(arg)) => arg,
            Ok(None) | Err(_) => continue,
        };
        if let Some(handle) = provider.refresh(max_age, arg) {
            started.push((provider.name().to_owned(), handle));
        }
    }
    started
}

#[cfg(test)]
mod models_arg_tests;
