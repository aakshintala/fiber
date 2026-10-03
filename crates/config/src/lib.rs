//! Reads the configuration files in Fiber home and the repository's `.fiber/`,
//! merges their layers and answers questions about the result
//! (`docs/configuration.md`). It is the only crate that reads configuration
//! files (`docs/architecture.md`, "The call rules"). It also finds Fiber home
//! (`docs/state.md`, "Override"), keeps secrets in `credentials/`, finds a
//! provider's key, reads an extension's manifest and provider data, and keeps
//! each provider's discovered model list in `cache/models/`.

mod cache;
mod credential;
mod error;
mod extension;
mod home;
mod keys;
mod path;
mod secret;
mod write;

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::events::Notice;
use serde_json::{Map, Value};

pub use cache::{read_model_cache, write_model_cache};
pub use error::ConfigError;
pub use extension::{
    Binary, Cost, Manifest, ModelData, Process, Protocol, ProviderData, Tier, read_manifest,
    read_providers,
};
pub use home::{ProjectKey, fiber_home, fiber_home_from_env};
pub use secret::{CredentialSource, Secret, read_secret, store_secret};
pub use write::{Scope, remove_extension_settings, set_global};

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

    /// Every layer merged, lowest first. With a model, each layer's
    /// `models."<model>"` wins over the same keys at that layer's top level
    /// ("Per model").
    pub fn merged(&self, model: Option<&str>) -> Value {
        let mut merged = Value::Object(Map::new());
        for (_, layer) in &self.layers {
            let upper = view(layer, model);
            // A provider's credential replaces the one below it as a whole
            // (docs/configuration.md, "Layers").
            for (name, provider) in upper
                .get("providers")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                if provider.get("credential").is_some()
                    && let Some(below) = merged
                        .get_mut("providers")
                        .and_then(|p| p.get_mut(name))
                        .and_then(Value::as_object_mut)
                {
                    below.remove("credential");
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
        let key = path::parse(key)?;
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
        write::update(&file, &key, value)?;
        self.settings_files
            .insert(file, cached.to_string().into_bytes());
        Ok(())
    }
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
    let Value::Object(map) = value else {
        return Err(top_level(&source.to_string()));
    };
    keys::check(map, source, notices).map(Value::Object)
}
