//! Reads the configuration files in Fiber home and the repository's `.fiber/`,
//! merges their layers and answers questions about the result
//! (`docs/configuration.md`). It is the only crate that reads configuration
//! files (`docs/architecture.md`, "The call rules"). It also finds Fiber home
//! (`docs/state.md`, "Override") and keeps secrets in `credentials/`.

mod error;
mod keys;
mod path;
mod secret;
mod write;

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::events::Notice;
use serde_json::{Map, Value};

pub use error::ConfigError;
pub use secret::{CredentialSource, Secret, read_secret, store_secret};
pub use write::{Scope, remove_extension_settings, set_extension_setting, set_global};

use keys::Ignored;

/// Where to read configuration from.
#[derive(Debug, Clone)]
pub struct Sources {
    /// Fiber home ([`fiber_home`]).
    pub home: PathBuf,
    /// The workspace, whose `.fiber/` is the repository layer.
    pub workspace: PathBuf,
    /// The project's key, naming `projects/<key>/` in Fiber home
    /// (`docs/state.md`, "Projects").
    pub project: String,
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

/// The merged configuration: every layer that was present, checked against
/// "Keys", lowest first.
#[derive(Debug, Clone)]
pub struct Config {
    sources: Sources,
    layers: Vec<(Source, Value)>,
    /// `-c extensions."<name>".settings.<key>=value`, by extension.
    run_settings: Map<String, Value>,
    notices: Vec<Notice>,
}

impl Config {
    /// Reads every layer. An unknown key, or a key a repository may not set,
    /// becomes a notice; invalid JSON or a wrongly typed value is an error.
    pub fn load(sources: Sources) -> Result<Self, ConfigError> {
        let mut run = Value::Object(Map::new());
        let mut run_settings = Map::new();
        for arg in &sources.overrides {
            let (key, text) = arg
                .split_once('=')
                .ok_or_else(|| ConfigError::Override { arg: arg.clone() })?;
            let key_path =
                path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
            let value = serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into()));
            match key_path.as_slice() {
                [area, name, settings, rest @ ..]
                    if area == "extensions" && settings == "settings" =>
                {
                    let settings = run_settings
                        .entry(name.clone())
                        .or_insert_with(|| Value::Object(Map::new()));
                    path::set(settings, rest, value);
                }
                _ => path::set(&mut run, &key_path, value),
            }
        }

        let home = &sources.home;
        let files = [
            (Source::Global(home.join("config.json")), false),
            (
                Source::Repository(sources.workspace.join(".fiber").join("config.json")),
                true,
            ),
            (
                Source::Project(
                    home.join("projects")
                        .join(&sources.project)
                        .join("config.json"),
                ),
                false,
            ),
        ];
        let mut notices = Vec::new();
        let mut layers = vec![(Source::Default, keys::defaults())];
        for (source, repo) in files {
            if let Some(file) = file_of(&source)
                && let Some(value) = read(file)?
            {
                let checked = check_layer(value, &source, repo, &mut notices)?;
                layers.push((source, checked));
            }
        }
        if !sources.overrides.is_empty() {
            let checked = check_layer(run, &Source::Run, false, &mut notices)?;
            layers.push((Source::Run, checked));
        }
        Ok(Self {
            sources,
            layers,
            run_settings,
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
            path::merge(&mut merged, &view(layer, model));
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

    /// An extension's settings, merged across its layers
    /// (`docs/configuration.md`, "Extension settings"). The repository's file
    /// may set only the keys the extension's manifest lists under
    /// `repo_settings`; any other key there is a notice and is ignored.
    pub fn extension_settings(
        &self,
        extension: &str,
        repo_settings: &[&str],
    ) -> Result<(Value, Vec<Notice>), ConfigError> {
        let home = &self.sources.home;
        let files = [
            (write::settings_file(home, extension), false),
            (
                write::settings_file(&self.sources.workspace.join(".fiber"), extension),
                true,
            ),
            (
                write::settings_file(
                    &home.join("projects").join(&self.sources.project),
                    extension,
                ),
                false,
            ),
        ];
        let mut merged = Value::Object(Map::new());
        let mut notices = Vec::new();
        for (file, repo) in files {
            let Some(value) = read(&file)? else {
                continue;
            };
            let Value::Object(mut map) = value else {
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

fn file_of(source: &Source) -> Option<&Path> {
    match source {
        Source::Global(file) | Source::Repository(file) | Source::Project(file) => Some(file),
        Source::Default | Source::Run => None,
    }
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
    repo: bool,
    notices: &mut Vec<Notice>,
) -> Result<Value, ConfigError> {
    let name = source.to_string();
    let Value::Object(map) = value else {
        return Err(top_level(&name));
    };
    let mut ignored = Vec::new();
    let checked = keys::check(map, repo, &mut ignored);
    notices.extend(ignored.into_iter().map(|dropped| {
        let why = match dropped {
            Ignored::Unknown(key) => format!("ignored `{key}`, which this Fiber does not know"),
            Ignored::PersonOnly(key) => format!("ignored `{key}`, which a repository may not set"),
        };
        Notice {
            code: ErrorCode::ConfigKeyIgnored,
            message: format!("{name}: {why}."),
            extension: None,
        }
    }));
    checked
        .map(Value::Object)
        .map_err(|wrong| ConfigError::WrongType {
            source_name: name,
            key: wrong.key,
            expected: wrong.expected,
        })
}

/// A file's JSON, or `None` when it does not exist.
fn read(file: &Path) -> Result<Option<Value>, ConfigError> {
    match fs::read(file) {
        Ok(bytes) => parse(file, &bytes).map(Some),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Io {
            file: file.to_path_buf(),
            source,
        }),
    }
}

/// Strict JSON: no comments, no trailing commas.
pub(crate) fn parse(file: &Path, bytes: &[u8]) -> Result<Value, ConfigError> {
    serde_json::from_slice(bytes).map_err(|e| ConfigError::Json {
        file: file.to_path_buf(),
        line: e.line(),
        column: e.column(),
    })
}

/// An extension's name as a file name: every `/` becomes `-`, as for
/// `extensions/` (`docs/state.md`, "What each part holds").
pub(crate) fn slug(name: &str) -> String {
    name.replace('/', "-")
}

/// Fiber home: `FIBER_HOME` when set, which must be an absolute path, or
/// `.fiber` in the home directory. A missing directory is created, mode 0700
/// (`docs/state.md`, "Override").
pub fn fiber_home(
    fiber_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    let dir = match fiber_home {
        Some(value) if value.is_empty() => {
            return Err(ConfigError::FiberHome(
                "FIBER_HOME is empty; set it to an absolute path or unset it.",
            ));
        }
        Some(value) => {
            let dir = PathBuf::from(value);
            if dir.is_relative() {
                return Err(ConfigError::FiberHome(
                    "FIBER_HOME must be an absolute path.",
                ));
            }
            dir
        }
        None => match home.map(PathBuf::from) {
            Some(home) if home.is_absolute() => home.join(".fiber"),
            Some(_) | None => {
                return Err(ConfigError::FiberHome(
                    "HOME is not an absolute path, so Fiber home is unknown; set FIBER_HOME.",
                ));
            }
        },
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|source| ConfigError::Io {
            file: dir.clone(),
            source,
        })?;
    Ok(dir)
}

/// [`fiber_home`] from the process's `FIBER_HOME` and `HOME`.
pub fn fiber_home_from_env() -> Result<PathBuf, ConfigError> {
    fiber_home(std::env::var_os("FIBER_HOME"), std::env::var_os("HOME"))
}
