//! Reads the configuration files in Fiber home and the repository's `.fiber/`,
//! merges their layers and answers questions about the result
//! (`docs/configuration.md`). It is the only crate that reads configuration
//! files (`docs/architecture.md`, "The call rules"). It also finds Fiber home
//! (`docs/state.md`, "Override"), keeps secrets in `credentials/`, finds a
//! provider's key, reads an extension's manifest and provider data, and keeps
//! each provider's discovered model list in `cache/models/`.

mod cache;
mod credential;
mod credential_file;
mod error;
mod extension;
mod home;
mod keys;
mod names;
mod path;
mod rules;
mod secret;
mod write;

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::events::Notice;
use serde_json::{Map, Value};

pub use cache::{model_cache_age, model_cache_lock_file, read_model_cache, write_model_cache};
pub use contract::Secret;
pub use credential::{Read, Runner};
pub use credential_file::{CredentialFile, CredentialLock};
pub use error::ConfigError;
pub use extension::{
    Binary, Cost, Manifest, ModelData, Opening, Placeholder, Process, Protocol, ProviderData, Tier,
    read_manifest, read_package_text, read_providers,
};
pub use home::{ProjectKey, fiber_home, fiber_home_from_env};
pub use names::{SHORT_NAMES, dir_name, full_name, short_name};
pub use rules::RulesFiles;
pub use secret::{
    CredentialSource, credential_labels, delete_credential, delete_credential_held,
    read_credential, read_secret, store_credential, store_secret,
};
pub use write::{Layer, Scope, remove_extension_settings, set, set_global, set_global_if_unset};

pub use keys::{diagnostics_debug, parse_duration, refresh_after};

use home::{parse, plain, read, read_bytes};

/// Where to read configuration from.
pub struct Sources {
    /// Fiber home ([`fiber_home`]).
    pub home: PathBuf,
    /// The workspace, whose `.fiber/` is the repository layer.
    pub workspace: PathBuf,
    /// The project, naming `projects/<key>/` in Fiber home.
    pub project: ProjectKey,
    /// Each `-c key=value` from the command line, in order.
    pub overrides: Vec<String>,
}

/// The layer a value came from (`docs/configuration.md`, "Layers").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Built-in defaults.
    Default,
    /// `config.json` at the top of Fiber home.
    Global(PathBuf),
    /// `.fiber/config.json` in the workspace.
    Repository(PathBuf),
    /// `projects/<key>/config.json` in Fiber home.
    Project(PathBuf),
    /// `-c key=value` on the command line.
    Run,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("the built-in defaults"),
            Self::Global(file) | Self::Repository(file) | Self::Project(file) => {
                write!(f, "{}", file.display())
            }
            Self::Run => f.write_str("-c"),
        }
    }
}

/// A session's configuration, read once: every layer that was present,
/// checked against "Keys", lowest first, and every extension settings file.
/// Nothing here is read again until the next load (`docs/configuration.md`,
/// "When Fiber reads configuration").
#[derive(Clone)]
pub struct Config {
    home: PathBuf,
    workspace: PathBuf,
    project: ProjectKey,
    layers: Vec<(Source, Value)>,
    /// `-c extensions."<name>".settings.<key>=value`, by extension.
    run_settings: Map<String, Value>,
    /// The bytes of every `config/<extension>.json` in each layer, by path.
    settings_files: BTreeMap<PathBuf, Vec<u8>>,
    notices: Vec<Notice>,
}

/// Shows where the configuration came from, never a value: a `-c` value or an
/// extension's setting may be something the person would not print.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sources: Vec<&Source> = self.layers.iter().map(|(source, _)| source).collect();
        f.debug_struct("Config")
            .field("layers", &sources)
            .field("settings_files", &self.settings_files.keys())
            .field("notices", &self.notices)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// Reads every layer. An unknown key, or a key a repository may not set,
    /// becomes a notice; invalid JSON or a wrongly typed value is an error.
    /// A repository's file that is a symbolic link or not a regular file is
    /// refused, since a repository is someone else's text.
    pub fn load(sources: Sources) -> Result<Self, ConfigError> {
        let mut run = Value::Object(Map::new());
        let mut run_settings = Map::new();
        for arg in &sources.overrides {
            let (key, text) = arg
                .split_once('=')
                .ok_or_else(|| ConfigError::Override { arg: arg.clone() })?;
            let bad = || ConfigError::Override { arg: key.into() };
            let key_path = path::parse(key).ok_or_else(bad)?;
            let value = serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into()));
            match key_path.as_slice() {
                [area, name, settings, rest @ ..]
                    if area == "extensions" && settings == "settings" =>
                {
                    if rest.is_empty() {
                        return Err(bad());
                    }
                    let settings = run_settings
                        .entry(name.clone())
                        .or_insert_with(|| Value::Object(Map::new()));
                    path::set(settings, rest, value);
                }
                _ => path::set(&mut run, &key_path, value),
            }
        }

        let run_settings = full_names(run_settings, &Source::Run, &["settings"])?;

        let home = sources.home;
        let fiber = sources.workspace.join(".fiber");
        let project_dir = home.join("projects").join(sources.project.as_str());
        let mut notices = Vec::new();
        let mut layers = vec![(Source::Default, keys::defaults())];
        let repository_file = fiber.join("config.json");
        plain(&fiber, true)?;
        let repository = if plain(&repository_file, false)? {
            read(&repository_file)?
        } else {
            None
        };
        let files = [
            (
                Source::Global(home.join("config.json")),
                read(&home.join("config.json"))?,
            ),
            (Source::Repository(repository_file), repository),
            (
                Source::Project(project_dir.join("config.json")),
                read(&project_dir.join("config.json"))?,
            ),
        ];
        for (source, value) in files {
            if let Some(value) = value {
                let checked = check_layer(value, &source, &mut notices)?;
                layers.push((source, checked));
            }
        }
        if !sources.overrides.is_empty() {
            let checked = check_layer(run, &Source::Run, &mut notices)?;
            layers.push((Source::Run, checked));
        }

        let mut settings_files = BTreeMap::new();
        for (dir, repo) in [(&home, false), (&fiber, true), (&project_dir, false)] {
            snapshot_settings(dir, repo, &mut settings_files)?;
        }
        Ok(Self {
            home,
            workspace: sources.workspace,
            project: sources.project,
            layers,
            run_settings,
            settings_files,
            notices,
        })
    }

    /// The keys that were ignored, one `config_key_ignored` notice each,
    /// naming the key and the file.
    pub fn notices(&self) -> &[Notice] {
        &self.notices
    }

    /// The workspace whose `.fiber/` is the repository layer.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The project, naming `projects/<key>/` in Fiber home.
    pub fn project(&self) -> &ProjectKey {
        &self.project
    }

    /// The merged value of `key` in `extension`'s settings, `None` when no
    /// layer sets it. `key` is a dotted key in the `path::parse` syntax
    /// `fiber config get` uses; the repository's file contributes only the
    /// keys `repo_settings` lists. Notices about ignored repository keys
    /// are dropped here; the session collects them once at load.
    pub fn extension_setting(
        &self,
        extension: &str,
        repo_settings: &[&str],
        key: &str,
    ) -> Result<Option<Value>, ConfigError> {
        let key = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
        // `extension_settings` reads its name as either spelling.
        let (merged, _) = self.extension_settings(extension, repo_settings)?;
        Ok(path::get(&merged, &key).cloned())
    }

    /// Every layer merged, lowest first. With a model, each layer's
    /// `models."<model>"` wins over the same keys at that layer's top level
    /// ("Per model").
    pub fn merged(&self, model: Option<&str>) -> Value {
        let mut merged = Value::Object(Map::new());
        for (_, layer) in &self.layers {
            let upper = view(layer, model);
            // Each entry under a provider's `credentials` replaces the one
            // below it as a whole (docs/configuration.md, "Layers").
            for (name, provider) in upper
                .get("providers")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                for label in provider
                    .get("credentials")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flat_map(Map::keys)
                {
                    if let Some(below) = merged
                        .get_mut("providers")
                        .and_then(|p| p.get_mut(name))
                        .and_then(|p| p.get_mut("credentials"))
                        .and_then(Value::as_object_mut)
                    {
                        below.remove(label);
                    }
                }
            }
            path::merge(&mut merged, &upper);
        }
        merged
    }

    /// The effective value of a dotted key and the layer it came from, as
    /// `fiber config get` prints them. An object merged from several layers
    /// names the highest.
    pub fn get(&self, key: &str, model: Option<&str>) -> Option<(Value, Source)> {
        let mut key = path::parse(key)?;
        if let [area, name, ..] = key.as_mut_slice()
            && area == "extensions"
        {
            *name = full_name(name);
        }
        let from = self
            .layers
            .iter()
            .rev()
            .find(|(_, layer)| path::get(&view(layer, model), &key).is_some());
        match from {
            Some((source, _)) => {
                let value = path::get(&self.merged(model), &key)?.clone();
                Some((value, source.clone()))
            }
            None => {
                let default = serde_json::from_str(keys::leaf(&key)?.default?).ok()?;
                Some((default, Source::Default))
            }
        }
    }

    /// Every name in the list at `key`, from every layer that sets it,
    /// lowest layer first, each name once at its first appearance: a list
    /// key whose layers all apply (`skills.disabled`). A key no layer sets
    /// gives an empty list.
    pub fn union_list(&self, key: &str) -> Vec<String> {
        let Some(key) = path::parse(key) else {
            return Vec::new();
        };
        let mut names: Vec<String> = Vec::new();
        for (_, layer) in &self.layers {
            let items = path::get(layer, &key).and_then(Value::as_array);
            for name in items.into_iter().flatten().filter_map(Value::as_str) {
                if !names.iter().any(|seen| seen == name) {
                    names.push(name.to_owned());
                }
            }
        }
        names
    }

    /// An extension's settings, merged across its layers as they were when
    /// this configuration was loaded, plus its own writes since
    /// (`docs/configuration.md`, "Extension settings"). The repository's file
    /// may set only the keys the extension's manifest lists under
    /// `repo_settings`; any other key there is a notice and is ignored.
    pub fn extension_settings(
        &self,
        extension: &str,
        repo_settings: &[&str],
    ) -> Result<(Value, Vec<Notice>), ConfigError> {
        let extension = &full_name(extension);
        let files = [
            (write::settings_file(&self.home, extension), false),
            (
                write::settings_file(&self.workspace.join(".fiber"), extension),
                true,
            ),
            (
                write::settings_file(
                    &self.home.join("projects").join(self.project.as_str()),
                    extension,
                ),
                false,
            ),
        ];
        let mut merged = Value::Object(Map::new());
        let mut notices = Vec::new();
        for (file, repo) in files {
            let Some(bytes) = self.settings_files.get(&file) else {
                continue;
            };
            let Value::Object(mut map) = parse(&file, bytes)? else {
                return Err(top_level(&file.display().to_string()));
            };
            if repo {
                map.retain(|key, _| {
                    let allowed = repo_settings.contains(&key.as_str());
                    if !allowed {
                        notices.push(Notice {
                            code: ErrorCode::ConfigKeyIgnored,
                            message: format!(
                                "{}: ignored `{key}`, which the extension does not list under repo_settings.",
                                file.display()
                            ),
                            extension: Some(extension.into()),
                        });
                    }
                    allowed
                });
            }
            path::merge(&mut merged, &Value::Object(map));
        }
        if let Some(run) = self.run_settings.get(extension) {
            path::merge(&mut merged, run);
        }
        Ok((merged, notices))
    }

    /// Sets one key in an extension's settings file (`host.config.set`). The
    /// value is visible to this configuration at once, and to other sessions
    /// at their next load ("When Fiber reads configuration").
    pub fn set_extension_setting(
        &mut self,
        extension: &str,
        scope: Scope,
        key: &str,
        value: Value,
    ) -> Result<(), ConfigError> {
        let extension = &full_name(extension);
        let dir = match scope {
            Scope::Machine => self.home.clone(),
            Scope::Project => self.home.join("projects").join(self.project.as_str()),
        };
        let file = write::settings_file(&dir, extension);
        let key = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
        // The session's copy is built first. A copy this session read as
        // invalid JSON fails the write before the disk is touched: "Invalid
        // JSON, or a value of the wrong type, is a startup error", and the
        // session sees a repaired file at its next reload.
        let mut cached = match self.settings_files.get(&file) {
            Some(bytes) => parse(&file, bytes)?,
            None => Value::Object(Map::new()),
        };
        path::set(&mut cached, &key, value.clone());
        // The file on disk may hold other sessions' writes too; this session
        // sees only its own until its next load.
        write::update(&file, &key, value, false)?;
        self.settings_files
            .insert(file, cached.to_string().into_bytes());
        Ok(())
    }
}

/// Whether `key` names a row of `docs/configuration.md`, "Keys", as
/// `fiber config get` checks it: any other key is a usage error.
pub fn known_key(key: &str) -> bool {
    path::parse(key).is_some_and(|segments| keys::leaf(&segments).is_some())
}

/// An extension package a repository ships: one entry of
/// `repository_extensions` (`docs/configuration.md`, "Keys").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryExtension {
    /// The package directory, as the repository wrote it.
    pub path: String,
    /// Whether the repository needs it.
    pub required: bool,
}

/// Everything a repository declares as code (`docs/extensions.md`, "Code a
/// repository ships"), read from the repository's own files alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Declared {
    /// `repository_extensions`.
    pub extensions: Vec<RepositoryExtension>,
    /// The `hooks` extension's `hooks` setting, by name.
    pub hooks: Map<String, Value>,
    /// `mcp.servers`, by name, with the keys a repository may set.
    pub mcp_servers: Map<String, Value>,
}

/// The `hooks` extension's name (`docs/extensions.md`, "Hooks declared in
/// configuration").
const HOOKS_EXTENSION: &str = "github.com/aakshintala/fiber/extensions/hooks";

/// Reads what the workspace's `.fiber/` declares, under the same plain-file
/// checks as [`Config::load`]. A repository with none gets an empty
/// [`Declared`].
pub fn declared(workspace: &Path) -> Result<Declared, ConfigError> {
    let fiber = workspace.join(".fiber");
    plain(&fiber, true)?;
    let mut declared = Declared::default();
    let file = fiber.join("config.json");
    if plain(&file, false)?
        && let Some(value) = read(&file)?
    {
        let Value::Object(layer) = value else {
            return Err(top_level(&file.display().to_string()));
        };
        let checked = keys::check(layer, &Source::Repository(file), &mut Vec::new())?;
        for item in checked
            .get("repository_extensions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(path) = item.get("path").and_then(Value::as_str) {
                declared.extensions.push(RepositoryExtension {
                    path: path.to_owned(),
                    required: item
                        .get("required")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
        }
        if let Some(Value::Object(servers)) = checked.get("mcp").and_then(|m| m.get("servers")) {
            declared.mcp_servers = servers.clone();
        }
    }
    plain(&fiber.join("config"), true)?;
    let hooks_file = write::settings_file(&fiber, HOOKS_EXTENSION);
    if plain(&hooks_file, false)?
        && let Some(bytes) = read_bytes(&hooks_file)?
    {
        let Value::Object(map) = parse(&hooks_file, &bytes)? else {
            return Err(top_level(&hooks_file.display().to_string()));
        };
        match map.get("hooks") {
            Some(Value::Object(hooks)) => declared.hooks = hooks.clone(),
            Some(_) => {
                return Err(ConfigError::WrongType {
                    source_name: hooks_file.display().to_string(),
                    key: "hooks".into(),
                    expected: "an object of hooks, one entry per name".into(),
                });
            }
            None => {}
        }
    }
    Ok(declared)
}

/// Reads every `*.json` in a layer directory's `config/` into `into`. In a
/// repository, the layer directory, `config/` and each file must be plain.
fn snapshot_settings(
    layer_dir: &Path,
    repo: bool,
    into: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), ConfigError> {
    let dir = layer_dir.join("config");
    if repo {
        plain(layer_dir, true)?;
        plain(&dir, true)?;
    }
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(ConfigError::Io { file: dir, source }),
    };
    for entry in entries {
        let file = entry
            .map_err(|source| ConfigError::Io {
                file: dir.clone(),
                source,
            })?
            .path();
        if file.extension().is_none_or(|e| e != "json") || (repo && !plain(&file, false)?) {
            continue;
        }
        if let Some(bytes) = read_bytes(&file)? {
            into.insert(file, bytes);
        }
    }
    Ok(())
}

/// A layer with its `models."<model>"` laid over its top level.
fn view(layer: &Value, model: Option<&str>) -> Value {
    let mut view = layer.clone();
    if let Some(model) = model
        && let Some(own) = layer.get("models").and_then(|m| m.get(model))
    {
        path::merge(&mut view, own);
    }
    view
}

fn top_level(source_name: &str) -> ConfigError {
    ConfigError::WrongType {
        source_name: source_name.into(),
        key: "(the whole file)".into(),
        expected: "an object".into(),
    }
}

fn check_layer(
    value: Value,
    source: &Source,
    notices: &mut Vec<Notice>,
) -> Result<Value, ConfigError> {
    let Value::Object(mut map) = value else {
        return Err(top_level(&source.to_string()));
    };
    if let Some(Value::Object(extensions)) = map.get_mut("extensions") {
        *extensions = full_names(std::mem::take(extensions), source, &[])?;
    }
    keys::check(map, source, notices).map(Value::Object)
}

/// Renames every extension in `by_name`, the object under
/// `extensions.<name>.<within>`, to its full name (`docs/configuration.md`,
/// "Keys"). Where both spellings of one extension appear, their objects
/// merge; one key set under both is an error.
fn full_names(
    by_name: Map<String, Value>,
    source: &Source,
    within: &[&str],
) -> Result<Map<String, Value>, ConfigError> {
    let mut out = Map::new();
    for (typed, value) in by_name {
        let name = full_name(&typed);
        match out.get_mut(&name) {
            None => {
                out.insert(name, value);
            }
            Some(existing) => {
                let mut at = vec!["extensions".to_owned(), short_name(&name).to_owned()];
                at.extend(within.iter().map(|s| (*s).to_owned()));
                merge_disjoint(existing, value, &mut at).map_err(|key| {
                    ConfigError::DuplicateExtension {
                        source_name: source.to_string(),
                        key,
                    }
                })?;
            }
        }
    }
    Ok(out)
}

/// Lays `upper` into `lower` where the two set different keys; the dotted
/// path of the first key both set is the error.
fn merge_disjoint(lower: &mut Value, upper: Value, at: &mut Vec<String>) -> Result<(), String> {
    match (lower, upper) {
        (Value::Object(below), Value::Object(above)) => {
            for (name, value) in above {
                match below.get_mut(&name) {
                    None => {
                        below.insert(name, value);
                    }
                    Some(existing) => {
                        at.push(name);
                        merge_disjoint(existing, value, at)?;
                        at.pop();
                    }
                }
            }
            Ok(())
        }
        _ => Err(path::display(at)),
    }
}
