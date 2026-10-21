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
mod credential_origin;
mod editable;
mod error;
mod extension;
mod extension_settings;
mod home;
mod import;
mod keys;
mod layout;
mod mcp_tokens;
mod names;
pub mod path;
mod prompt_files;
mod rules;
mod secret;
mod settings;
mod startup_check;
mod switches;
mod themes;
mod write;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use contract::events::Notice;
use serde_json::{Map, Value};

pub use cache::{
    cached_model_lists, model_cache_age, model_cache_lock_file, read_model_cache, write_model_cache,
};
pub use contract::Secret;
pub use contract::provider::{Cost, Tier};
pub use credential::{Credentials, Read, Runner};
pub use credential_file::{CredentialFile, CredentialLock};
pub use credential_origin::{Origin, Presence};
pub use editable::is_editable_config_path;
pub use error::ConfigError;
pub use extension::{
    Binary, CredentialFrom, CredentialLabelSources, Login, Manifest, ModelData, Opening,
    Placeholder, Process, Protocol, ProviderData, has_manifest, provider_files, read_manifest,
    read_package_text, read_providers,
};
pub use extension_settings::ExtensionSettings;
pub use home::{
    ProjectKey, create_fiber_home, fiber_home, fiber_home_from_env, fiber_home_path,
    fiber_home_path_from_env,
};
pub use import::{
    ImportReport, ImportSource, Imported, LeftBehind, import_pending, import_source, source_home,
};
pub use layout::project_servers_dir;
pub use layout::{
    crashes_dir, docs_dir, extension_data_dir, extensions_dir, global_config_file,
    images_cache_dir, logs_dir, machine_approvals_dir, mcp_cache_dir, pinned_dir,
    project_approvals_dir, project_config_file, project_data_dir, repository_config_file, run_dir,
};
pub use mcp_tokens::McpTokenFiles;
pub use names::{SHORT_NAMES, dir_name, full_name, short_name};
pub use prompt_files::{append_prompt, system_prompt};
pub use rules::{RuleLine, RulesFiles, RulesListing, RulesScope, list_rules, remove_rule};
pub use secret::{
    CredentialSource, credential_labels, delete_credential, delete_credential_held, delete_secret,
    mcp_secret_name_ok, read_credential, read_secret, store_credential, store_secret,
    valid_env_key, valid_server_name,
};
pub use settings::{NotesFile, ReviewerNotes, SettingInfo, SettingValue, reviewer_notes};
pub use startup_check::{
    McpRefusal, McpRefusalKind, McpServer, McpServers, McpTransport, ScriptRead, mcp_server_key,
    mcp_servers, quoted_segment, read_script,
};
pub use switches::{SwitchLayer, ToolOwner, ToolSwitch, switch_skill, switch_tool};
pub use themes::{theme_names, theme_text};
pub use write::{
    FileOutcome, FileSaved, Layer, ListChange, ListEdit, NotesSaved, NotesWrite, Scope,
    clear_global_if_equals, edit_list, get_global, insert_entry, remove_entry,
    remove_extension_settings, replace_global, save_reviewer_notes, set, set_global_if_unset,
    settings_file, update_global_entries, version, write_atomic,
};

pub use keys::{WriteScope, diagnostics_debug, parse_duration, refresh_after};

use home::{parse, plain, read, read_bytes};

/// Where to read configuration from.
#[derive(Debug, Clone)]
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

impl Source {
    /// The write layer this source's file is, if any: the built-in
    /// defaults and `-c` flags name no file.
    pub fn layer(&self) -> Option<Layer> {
        match self {
            Self::Global(_) => Some(Layer::Global),
            Self::Project(_) => Some(Layer::Project),
            Self::Repository(_) => Some(Layer::Repository),
            Self::Default | Self::Run => None,
        }
    }
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
/// checked against "Keys", lowest first, one extension's settings, and
/// one provider subtree's credentials. Nothing here is read again until
/// the next load (`docs/configuration.md`, "When Fiber reads
/// configuration").
#[derive(Clone)]
pub struct Config {
    layers: Vec<(Source, Value)>,
    notices: Vec<Notice>,
    extensions: ExtensionSettings,
    credentials: Credentials,
}

/// Shows where the configuration came from, never a value: a `-c` value, a
/// credential source's arguments or an extension's setting may be something
/// the person would not print.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sources: Vec<&Source> = self.layers.iter().map(|(source, _)| source).collect();
        f.debug_struct("Config")
            .field("layers", &sources)
            .field("extensions", &self.extensions)
            .field("notices", &self.notices)
            .finish_non_exhaustive()
    }
}

/// What the fast path reads without merging every layer.
enum Fast {
    /// The answer: `get_segments` returns it directly.
    Answer(Option<(Value, Source)>),
    /// The key needs the whole merge: `get_segments` reads `merged`.
    Merge,
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
        let (repository_file, repository) = read_repository(&sources.workspace)?;
        let global_file = global_config_file(&home);
        let project_file = project_config_file(&home, &sources.project);
        let files = [
            (Source::Global(global_file.clone()), read(&global_file)?),
            (Source::Repository(repository_file), repository),
            (Source::Project(project_file.clone()), read(&project_file)?),
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
        let extensions = ExtensionSettings::new(
            home.clone(),
            sources.workspace.clone(),
            sources.project.clone(),
            run_settings,
            settings_files,
        );
        let providers = merge_layers(&layers, None)
            .get("providers")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        Ok(Self {
            layers,
            notices,
            extensions,
            credentials: Credentials::new(home, providers, credential::CommandRuns::default()),
        })
    }

    /// The keys that were ignored, one `config_key_ignored` notice each,
    /// naming the key and the file.
    pub fn notices(&self) -> &[Notice] {
        &self.notices
    }

    /// One extension's settings files.
    pub fn extensions(&self) -> &ExtensionSettings {
        &self.extensions
    }

    /// One extension's settings files, to write through.
    pub fn extensions_mut(&mut self) -> &mut ExtensionSettings {
        &mut self.extensions
    }

    /// One provider subtree's credentials.
    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// Every layer merged, lowest first. With a model, each layer's
    /// `models."<model>"` wins over the same keys at that layer's top level
    /// ("Per model"). Each key combines as its row says
    /// (`docs/configuration.md`, "Layers"): a union key holds every
    /// layer's names, a credential entry is replaced whole, and any other
    /// value replaces the one below it, an object key by key.
    pub fn merged(&self, model: Option<&str>) -> Value {
        let mut merged = merge_layers(&self.layers, model);
        for key in keys::union_rows() {
            let names: Vec<Value> = self
                .unioned(&key, model)
                .into_iter()
                .map(|(name, _)| Value::String(name))
                .collect();
            path::set(&mut merged, &key, Value::Array(names));
        }
        merged
    }

    /// The effective value of a dotted key and the layer it came from, as
    /// `fiber config get` prints them. An object merged from several layers
    /// names the highest.
    pub fn get(&self, key: &str, model: Option<&str>) -> Option<(Value, Source)> {
        self.get_segments(&path::parse(key)?, model)
    }

    /// The effective value and source of a key supplied as literal segments,
    /// with the same layering and extension aliases as [`Self::get`].
    pub fn get_segments(
        &self,
        segments: &[String],
        model: Option<&str>,
    ) -> Option<(Value, Source)> {
        let mut key = segments.to_vec();
        if let [area, name, ..] = key.as_mut_slice()
            && area == "extensions"
        {
            *name = full_name(name);
        }
        if let Fast::Answer(found) = self.fast_get(&key, model) {
            return found;
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

    /// The value and source of `key` without merging every layer. A
    /// scalar reads whole from the highest layer holding it; an object
    /// merges only its subtree, top-down over the layers holding it.
    /// `Fast::Merge` reads through `merged`: a union or entry-replaced
    /// row at `key`, above it or below its fill, a key a higher layer
    /// hides, or a layer whose per-model view is not an object.
    fn fast_get(&self, key: &[String], model: Option<&str>) -> Fast {
        // Only a plain replace row merges holder-wins: a union row
        // holds every layer's names, and an entry-replaced row goes
        // whole to one layer, neither what the highest layer holds.
        if (0..=key.len()).any(|len| {
            key.get(..len).is_some_and(|prefix| {
                keys::leaf(prefix).is_some_and(|row| row.merge != keys::Merge::Replace)
            })
        }) {
            return Fast::Merge;
        }
        // Below a union row's fill the merge re-creates the object, so
        // a proper prefix of one reads what no single layer holds. The
        // row itself needs no length check: it already returned above.
        if keys::union_rows().any(|row| row.starts_with(key)) {
            return Fast::Merge;
        }
        // A layer whose per-model view is not an object wipes the merge,
        // so it reads through `merged`.
        let views: Vec<Cow<'_, Value>> = self
            .layers
            .iter()
            .map(|(_, layer)| view(layer, model))
            .collect();
        if !views.iter().all(|seen| seen.is_object()) {
            return Fast::Merge;
        }
        // Top-down: the first layer holding the key is the highest,
        // unless a higher layer holds a scalar over its path, hiding it
        // as the merge does.
        let mut hiding = false;
        for ((source, _), seen) in self.layers.iter().zip(&views).rev() {
            if let Some(value) = path::get(seen, key) {
                if hiding {
                    return Fast::Answer(None);
                }
                if value.is_object() {
                    // Objects merge key by key across the layers holding
                    // them: merge only this subtree, top-down, as `lay`
                    // merges it, union and entry-replaced rows included.
                    let mut sub = Value::Object(Map::new());
                    for (_, seen) in self.layers.iter().zip(&views) {
                        if let Some(upper) = path::get(seen, key) {
                            lay(&mut sub, upper, &mut key.to_vec());
                        }
                    }
                    return Fast::Answer(
                        path::get(&sub, key)
                            .cloned()
                            .map(|found| (found, source.clone())),
                    );
                }
                return Fast::Answer(Some((value.clone(), source.clone())));
            }
            if (1..key.len()).any(|len| {
                key.get(..len)
                    .is_some_and(|prefix| path::get(seen, prefix).is_some_and(|at| !at.is_object()))
            }) {
                hiding = true;
            }
        }
        // No layer holds the key: the schema default.
        Fast::Answer(
            keys::leaf(key)
                .and_then(|row| row.default)
                .and_then(|text| serde_json::from_str(text).ok())
                .map(|default| (default, Source::Default)),
        )
    }

    /// The effective value of `key` as a whole number of zero or more:
    /// `None` for a missing key without a default, or a value of another
    /// JSON type (a negative or fractional number is `None`).
    pub fn count(&self, key: &str, model: Option<&str>) -> Option<u64> {
        self.get(key, model).and_then(|(value, _)| value.as_u64())
    }

    /// The effective value of `key` as a flag: `None` for a missing key
    /// without a default, or a value of another JSON type.
    pub fn flag(&self, key: &str, model: Option<&str>) -> Option<bool> {
        self.get(key, model).and_then(|(value, _)| value.as_bool())
    }

    /// The effective value of `key` as text: `None` for a missing key
    /// without a default, or a value of another JSON type.
    pub fn text(&self, key: &str, model: Option<&str>) -> Option<String> {
        self.get(key, model)
            .and_then(|(value, _)| value.as_str().map(str::to_owned))
    }

    /// The effective value of `key` as a list of strings, in order,
    /// skipping non-strings: empty for a missing key or a non-array.
    pub fn list(&self, key: &str, model: Option<&str>) -> Vec<String> {
        self.get(key, model)
            .and_then(|(value, _)| value.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|name| name.as_str().map(str::to_owned))
            .collect()
    }

    /// Every name in the list at `key`, from every layer that sets it,
    /// lowest layer first, each name once at its first appearance: a list
    /// key whose layers all apply (`skills.disabled`). A key no layer sets
    /// gives an empty list.
    pub fn union_list(&self, key: &str) -> Vec<String> {
        let Some(key) = path::parse(key) else {
            return Vec::new();
        };
        self.unioned(&key, None)
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// The names at `key` with their source. A union carries each name's
    /// lowest layer; any other merge kind carries the effective list's
    /// highest layer. The key's `Merge` row selects which behavior applies.
    /// Reads each layer through its per-model view, as `merged` does, so a
    /// model view and the raw layer never disagree.
    pub(crate) fn unioned(&self, key: &[String], model: Option<&str>) -> Vec<(String, Source)> {
        let Some(row) = keys::leaf(key) else {
            return Vec::new();
        };
        if row.merge != keys::Merge::Union {
            let Some((value, source)) = self.get(&path::display(key), model) else {
                return Vec::new();
            };
            return value
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|name| (name.to_owned(), source.clone()))
                .collect();
        }

        let mut names: Vec<(String, Source)> = Vec::new();
        for (source, layer) in &self.layers {
            let seen = view(layer, model);
            let items = path::get(&seen, key).and_then(Value::as_array);
            for name in items.into_iter().flatten().filter_map(Value::as_str) {
                if !names.iter().any(|(seen, _)| seen == name) {
                    names.push((name.to_owned(), source.clone()));
                }
            }
        }
        names
    }

    /// The person's `reviewer.context` notes as the reviewer reads them
    /// (`docs/permissions.md`, "What the person tells it"): the global
    /// text under its heading, then the per-project text under its own.
    /// A layer with no text, or only whitespace, is skipped. Empty where
    /// neither layer sets any.
    pub fn reviewer_context(&self) -> String {
        const EVERYWHERE: &str = "## Notes that hold everywhere";
        const PROJECT: &str = "## Notes for this project";
        // The person's own files alone: no `-c` flag, repository or
        // default ever reaches the render.
        let key = ["reviewer".to_owned(), "context".to_owned()];
        let mut global = None;
        let mut project = None;
        for (source, layer) in &self.layers {
            let text = path::get(layer, &key).and_then(Value::as_str);
            match source {
                Source::Global(_) => global = text,
                Source::Project(_) => project = text,
                Source::Default | Source::Repository(_) | Source::Run => {}
            }
        }
        [(EVERYWHERE, global), (PROJECT, project)]
            .into_iter()
            .filter_map(|(heading, text)| {
                let text = text?.trim_end();
                (!text.trim().is_empty()).then(|| format!("{heading}\n\n{text}"))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
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
    let mut declared = Declared::default();
    let (file, repository) = read_repository(workspace)?;
    if let Some(value) = repository {
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
            // An entry with no value is not a declaration: a repository
            // file holding only emptied objects (such as a `tools` subtree
            // whose hints were dropped) neither offers nor gates anything.
            declared.mcp_servers = servers
                .iter()
                .filter(|(_, entry)| has_value(entry))
                .map(|(name, entry)| (name.clone(), entry.clone()))
                .collect();
        }
    }
    let mut settings_files = BTreeMap::new();
    snapshot_settings(&workspace.join(".fiber"), true, &mut settings_files)?;
    let hooks_file = write::settings_file(&workspace.join(".fiber"), HOOKS_EXTENSION);
    if let Some(bytes) = settings_files.get(&hooks_file) {
        let Value::Object(map) = parse(&hooks_file, bytes)? else {
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

/// Whether a declared entry holds at least one value: anything that is
/// not an object, at any depth. An entry of only emptied objects is not
/// a declaration.
fn has_value(entry: &Value) -> bool {
    match entry {
        Value::Object(map) => map.values().any(has_value),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            true
        }
    }
}

/// The workspace's `.fiber/config.json`: plain-file checks, then the
/// parsed value, shared by [`Config::load`] and [`declared`]. `None` when
/// there is no such file.
fn read_repository(workspace: &Path) -> Result<(PathBuf, Option<Value>), ConfigError> {
    let fiber = workspace.join(".fiber");
    plain(&fiber, true)?;
    let file = repository_config_file(workspace);
    let value = if plain(&file, false)? {
        read(&file)?
    } else {
        None
    };
    Ok((file, value))
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

/// Every layer in `layers` merged, lowest first, each with its
/// `models."<model>"` laid over its top level.
fn merge_layers(layers: &[(Source, Value)], model: Option<&str>) -> Value {
    let mut merged = Value::Object(Map::new());
    for (_, layer) in layers {
        lay(&mut merged, &view(layer, model), &mut Vec::new());
    }
    merged
}

/// Lays `upper` over `merged` as one row's merge kind says: a credential
/// entry replaces the one below as a whole, and any other value replaces
/// the one below it, an object key by key. Union keys are skipped: after
/// the walk `merged` fills them from `unioned`, so every reader shares one
/// table-selected union.
fn lay(merged: &mut Value, upper: &Value, path: &mut Vec<String>) {
    if let Some(row) = keys::leaf(path)
        && row.merge == keys::Merge::EntryReplace
    {
        path::set(merged, path, upper.clone());
        return;
    }
    match upper {
        Value::Object(map) if map.is_empty() => {
            // An emptied object still names its path: the layers below
            // kept nothing under it, but it is set, not dropped.
            if path::get(merged, path).is_none() {
                path::set(merged, path, Value::Object(Map::new()));
            }
        }
        Value::Object(map) => {
            for (name, value) in map {
                path.push(name.clone());
                lay(merged, value, path);
                path.pop();
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            path::set(merged, path, upper.clone())
        }
    }
}

/// A layer with its `models."<model>"` laid over its top level. Borrows
/// the layer when there is nothing to lay over it: no model, or no entry
/// for the model. Any entry at `models.<model>`, an object or not, is
/// merged over a clone.
fn view<'a>(layer: &'a Value, model: Option<&str>) -> Cow<'a, Value> {
    match model.and_then(|model| layer.get("models").and_then(|models| models.get(model))) {
        None => Cow::Borrowed(layer),
        Some(own) => {
            let mut view = layer.clone();
            path::merge(&mut view, own);
            Cow::Owned(view)
        }
    }
}

pub(crate) fn top_level(source_name: &str) -> ConfigError {
    ConfigError::WrongType {
        source_name: source_name.into(),
        key: "(the whole file)".into(),
        expected: "an object".into(),
    }
}

pub(crate) fn check_layer(
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
