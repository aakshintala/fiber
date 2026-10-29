//! Writing configuration (`docs/configuration.md`, "When Fiber writes"): take
//! the lock, read the file, change one key, and rename a temporary file over
//! it (`docs/state.md`, "Concurrent access").

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use contract::events::Notice;

use self::atomic::locked;
use crate::error::ConfigError;
use crate::extension::{declared_settings_in, host_shape, installed_dir};
use crate::extension_settings::settings_key;
use crate::home::{ProjectKey, plain, read};
use crate::layout::{global_config_file, project_config_file, repository_config_file};
use crate::{Source, keys, path};

/// Which extension settings file `host.config.set` writes: the same words
/// `host.data_dir` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `config/<extension>.json` in Fiber home.
    Machine,
    /// `projects/<key>/config/<extension>.json` in Fiber home.
    Project,
}

/// Which configuration file `set` writes (`docs/configuration.md`, "When
/// Fiber writes").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// `config.json` at the top of Fiber home.
    Global,
    /// `projects/<key>/config.json` in Fiber home.
    Project,
    /// The workspace's `.fiber/config.json`.
    Repository,
}

/// Sets one key in a layer's file (`fiber config set`). The value's type is
/// checked against "Keys", and a key the
/// layer may not hold is refused before anything is written: on any `Err`
/// no file is created or changed. Returns the layer's file it wrote.
pub fn set(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: Layer,
    key: &str,
    value: Value,
) -> Result<PathBuf, ConfigError> {
    // An extension's own setting never lives in `config.json`: route
    // every `extensions.<name>.settings` shape, the empty rest included,
    // to the settings write, which holds every gate in one order.
    if let Some((extension, relative)) = settings_key(key) {
        return set_extension_setting(
            home, workspace, project, layer, key, &extension, &relative, value, None,
        );
    }
    let (file, source) = layer_file(home, workspace, project, layer)?;
    let (segments, notices) = checked(key, value.clone(), &source)?;
    if !notices.is_empty() {
        return Err(ConfigError::Refused {
            key: key.into(),
            file,
            why: refused_why(&segments, &source),
        });
    }
    let _lock = locked(&file)?;
    let mut root = read(&file)?.unwrap_or_else(|| Value::Object(Map::new()));
    // Equivalent names may hold disjoint leaves, never the same leaf twice.
    if let Some(Value::Object(extensions)) = root.get("extensions") {
        crate::full_names(extensions.clone(), &source, &[])?;
    }
    let resolved = resolve_spelling(&root, &segments);
    path::set(&mut root, &resolved, value);
    write_root(&file, &root)?;
    Ok(file)
}

/// Why an extension-settings write is refused: one static reason per
/// gate, so no exhaustive match in the repo gains a case. The refusal's
/// key names the typed extension; no reason ever repeats a value.
/// The extension is neither installed nor approved where `set` can see.
const UNINSTALLED: &str = "the extension is not installed";
/// The key's first segment is in neither declared list.
const UNKNOWN_SETTING: &str = "the extension declares no such setting";
/// A repository's file names a key outside the manifest's list.
const NOT_REPO_SETTING: &str = "the extension does not list it under repo_settings";
/// Below a placeholder name sits an object, never a host.
const PLACEHOLDER_DESCENDANT: &str = "a placeholder setting holds no object below it";
/// A single-segment placeholder key holds no host.
const NOT_A_HOST: &str = "the value is not a host";

/// Writes one extension-settings key for a global or project layer, or
/// refuses a repository-layer key `set` cannot resolve: only the narrow
/// [`set_repo_extension_setting`] takes the caller-resolved
/// approved-or-installed directory, since this crate never resolves an
/// approved copy itself. `extension_dir` is `Some` only for the narrow
/// repository path; `None` resolves the installed copy internally.
#[allow(
    clippy::too_many_arguments,
    reason = "one shared edge for every settings write; the narrow repository path adds its directory"
)]
fn set_extension_setting(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: Layer,
    key: &str,
    extension: &str,
    relative: &[String],
    value: Value,
    extension_dir: Option<&Path>,
) -> Result<PathBuf, ConfigError> {
    // The empty rest is usage, as an empty `-c` rest is.
    let [first, ..] = relative else {
        return Err(ConfigError::Override { arg: key.into() });
    };
    let repository = matches!(layer, Layer::Repository);
    if repository && extension_dir.is_none() {
        // No caller passes a directory for any other layer, and this
        // crate resolves no approved copy itself: a repository key
        // reaching `set` directly names no directory this crate may use.
        return Err(ConfigError::Refused {
            key: key.into(),
            file: settings_file(&workspace.join(".fiber"), extension),
            why: UNINSTALLED,
        });
    }
    if repository {
        // Someone else's text: a link, or anything but a directory and
        // a regular file, is refused before the lock is taken, as
        // `Config::load` refuses to read it.
        plain(&workspace.join(".fiber"), true)?;
        plain(&workspace.join(".fiber").join("config"), true)?;
        plain(&settings_file(&workspace.join(".fiber"), extension), false)?;
    }
    let dir = match extension_dir {
        Some(dir) => dir.to_path_buf(),
        None => {
            let file = settings_layer_file(home, workspace, project, layer, extension);
            installed_dir(home, extension).ok_or_else(|| ConfigError::Refused {
                key: key.into(),
                file,
                why: UNINSTALLED,
            })?
        }
    };
    // An unreadable manifest or provider file fails the write before
    // anything is written.
    let declared = declared_settings_in(&dir)?;
    let file = settings_layer_file(home, workspace, project, layer, extension);
    // A placeholder's setting is never a `repo_settings` key: the
    // repository refuses it even when the manifest lists it there, since
    // `declared_settings_in` already keeps it out of `repo_settings`.
    if repository
        && (declared.placeholders.contains(first)
            || !declared.repo_settings.iter().any(|name| name == first))
    {
        return Err(ConfigError::Refused {
            key: key.into(),
            file: file.clone(),
            why: NOT_REPO_SETTING,
        });
    }
    let declared_names: Vec<&str> = declared
        .repo_settings
        .iter()
        .map(String::as_str)
        .chain(declared.placeholders.iter().map(String::as_str))
        .collect();
    if !declared_names.contains(&first.as_str()) {
        return Err(ConfigError::Refused {
            key: key.into(),
            file: file.clone(),
            why: UNKNOWN_SETTING,
        });
    }
    if declared.placeholders.contains(first) && relative.len() > 1 {
        return Err(ConfigError::Refused {
            key: key.into(),
            file: file.clone(),
            why: PLACEHOLDER_DESCENDANT,
        });
    }
    if declared.placeholders.contains(first)
        && relative.len() == 1
        && !matches!(&value, Value::String(text) if host_shape(text).is_some())
    {
        return Err(ConfigError::Refused {
            key: key.into(),
            file: file.clone(),
            why: NOT_A_HOST,
        });
    }
    set_settings_file(&file, relative, value)?;
    Ok(file)
}

/// The settings file `layer` writes for `extension`: `config/<slug>.json`
/// under Fiber home, the workspace's `.fiber/`, or the project.
fn settings_layer_file(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: Layer,
    extension: &str,
) -> PathBuf {
    match layer {
        Layer::Global => settings_file(home, extension),
        Layer::Project => settings_file(&home.join("projects").join(project.as_str()), extension),
        Layer::Repository => settings_file(&workspace.join(".fiber"), extension),
    }
}

/// Writes one extension's settings key for the repository layer only,
/// against the caller-resolved approved-or-installed directory: the
/// approved copy wins, the personal install is the fallback, and neither
/// means uninstalled and refused by the caller. Holds the same gates in
/// the same order as the global and project path. Never resolves a
/// directory itself.
///
/// Returns the settings file it wrote: `.fiber/config/<slug>.json`.
pub fn set_repo_extension_setting(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    extension_dir: &Path,
    key: &str,
    value: Value,
) -> Result<PathBuf, ConfigError> {
    let Some((extension, relative)) = settings_key(key) else {
        return Err(ConfigError::Override { arg: key.into() });
    };
    set_extension_setting(
        home,
        workspace,
        project,
        Layer::Repository,
        key,
        &extension,
        &relative,
        value,
        Some(extension_dir),
    )
}

/// Reads the settings file under its lock, sets one relative key and
/// writes the whole file back. A concurrent write that replaced the whole
/// document with a non-object shape fails with the `top_level` shape
/// error, leaving the file byte-identical.
fn set_settings_file(file: &Path, relative: &[String], value: Value) -> Result<(), ConfigError> {
    let _lock = locked(file)?;
    let mut root = match read(file)? {
        None => Value::Object(Map::new()),
        Some(Value::Object(map)) => Value::Object(map),
        Some(_) => return Err(crate::top_level(&file.display().to_string())),
    };
    path::set(&mut root, relative, value);
    write_root(file, &root)?;
    Ok(())
}

/// One change to a list of names in one layer's file (`/tools`, `/skills`).
pub struct ListEdit<'a> {
    /// The list's dotted key.
    pub key: &'a str,
    /// The name to add or remove.
    pub name: &'a str,
    /// What to do to the list.
    pub change: ListChange,
    /// The list in force below this file, when its file sets none.
    pub inherited: Option<&'a [String]>,
}

/// What a [`ListEdit`] does to its list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListChange {
    /// Adds the name, starting from the inherited list (or none) when the file holds no list.
    Add,
    /// Removes every copy of the name, starting likewise.
    Remove,
    /// Adds the name only when a list is in force (the file's or the
    /// inherited one) and lacks it: `tools.enabled` when a tool is switched
    /// on, decided under the file's lock.
    AddIfListed,
}

/// Applies `edits` to `layer`'s file under one lock and one write; `true`
/// when it wrote. A failure before the rename leaves the file unchanged; a
/// failure after it, syncing the directory, returns `Err` with the new
/// content already in place.
pub fn edit_list(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: Layer,
    edits: &[ListEdit<'_>],
) -> Result<bool, ConfigError> {
    if edits.is_empty() {
        return Ok(false);
    }
    let (file, source) = layer_file(home, workspace, project, layer)?;
    let _lock = locked(&file)?;
    let mut root = read(&file)?.unwrap_or_else(|| Value::Object(Map::new()));
    let mut changed = false;
    for edit in edits {
        if apply_edit(&mut root, edit, &source, &file)? {
            changed = true;
        }
    }
    if !changed {
        return Ok(false);
    }
    write_root(&file, &root)?;
    Ok(true)
}

/// Applies one edit to the file's in-memory root; whether its list changed.
/// Every edit is type-checked as `set` checks it, changed or not, so a key
/// the layer may not set is refused even when it would change nothing.
fn apply_edit(
    root: &mut Value,
    edit: &ListEdit<'_>,
    source: &Source,
    file: &Path,
) -> Result<bool, ConfigError> {
    let segments = path::parse(edit.key).ok_or_else(|| ConfigError::Override {
        arg: edit.key.into(),
    })?;
    let resolved = resolve_spelling(root, &segments);
    let shown = path::display(&resolved);
    // The file's list at the leaf, else the one the file inherits.
    let current: Option<Vec<String>> = match path::get(root, &resolved) {
        None => edit.inherited.map(|list| list.to_vec()),
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        ),
        Some(other) => {
            check_list(&shown, edit.key, &resolved, other.clone(), source, file)?;
            None
        }
    };
    let name = edit.name.to_owned();
    match edit.change {
        ListChange::Add => {
            let mut list = current.clone().unwrap_or_default();
            if list.iter().any(|n| n == edit.name) {
                check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
                return Ok(false);
            }
            list.push(name);
            check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
            path::set(root, &resolved, Value::Array(list_into(list)));
            Ok(true)
        }
        ListChange::Remove => {
            let mut list = current.clone().unwrap_or_default();
            if !list.iter().any(|n| n == edit.name) {
                check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
                return Ok(false);
            }
            list.retain(|n| n != edit.name);
            check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
            path::set(root, &resolved, Value::Array(list_into(list)));
            Ok(true)
        }
        ListChange::AddIfListed => match current {
            Some(mut list) if !list.iter().any(|n| n == edit.name) => {
                list.push(name);
                check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
                path::set(root, &resolved, Value::Array(list_into(list)));
                Ok(true)
            }
            Some(list) => {
                check_list(&shown, edit.key, &resolved, strings_of(&list), source, file)?;
                Ok(false)
            }
            None => {
                check_list(
                    &shown,
                    edit.key,
                    &resolved,
                    Value::Array(Vec::new()),
                    source,
                    file,
                )?;
                Ok(false)
            }
        },
    }
}

/// The leaf list the file already holds under one spelling of an
/// extension's name keeps that spelling; a new list joins an existing
/// spelling of the same extension; else the key's own spelling stands. One
/// leaf is resolved at a time, so two lists may keep different spellings
/// without one key ever being set under both (`docs/configuration.md`,
/// "Keys").
fn resolve_spelling(root: &Value, segments: &[String]) -> Vec<String> {
    let [area, name, rest @ ..] = segments else {
        return segments.to_vec();
    };
    if area != "extensions" {
        return segments.to_vec();
    }
    let mut same: Vec<String> = root
        .get("extensions")
        .and_then(Value::as_object)
        .map(|extensions| {
            extensions
                .keys()
                .filter(|key| crate::names::full_name(key) == crate::names::full_name(name))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    same.sort();
    let holds = |key: &String| {
        let mut at = vec!["extensions".to_owned(), key.clone()];
        at.extend(rest.iter().cloned());
        path::get(root, &at).is_some()
    };
    let chosen = same.iter().find(|key| holds(key)).or_else(|| same.first());
    match chosen {
        Some(key) => {
            let mut resolved = vec!["extensions".to_owned(), key.clone()];
            resolved.extend(rest.iter().cloned());
            resolved
        }
        None => segments.to_vec(),
    }
}

/// Type-checks `value` for `shown` as `set` checks it: a notice is the same
/// refusal `set` returns.
fn check_list(
    shown: &str,
    key: &str,
    segments: &[String],
    value: Value,
    source: &Source,
    file: &Path,
) -> Result<(), ConfigError> {
    let (_, notices) = checked(shown, value, source)?;
    if notices.is_empty() {
        return Ok(());
    }
    Err(ConfigError::Refused {
        key: key.into(),
        file: file.to_path_buf(),
        why: refused_why(segments, source),
    })
}

fn strings_of(list: &[String]) -> Value {
    Value::Array(
        list.iter()
            .map(|name| Value::String(name.clone()))
            .collect(),
    )
}

fn list_into(list: Vec<String>) -> Vec<Value> {
    list.into_iter().map(Value::String).collect()
}

impl Layer {
    /// The source reading this layer's file.
    pub(crate) fn source(self, home: &Path, workspace: &Path, project: &ProjectKey) -> Source {
        match self {
            Self::Global => Source::Global(global_config_file(home)),
            Self::Project => Source::Project(project_config_file(home, project)),
            Self::Repository => Source::Repository(repository_config_file(workspace)),
        }
    }
}

/// The file and source `set` and `edit_list` write through for `layer`.
fn layer_file(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: Layer,
) -> Result<(PathBuf, Source), ConfigError> {
    let source = layer.source(home, workspace, project);
    let (Source::Global(file) | Source::Repository(file) | Source::Project(file)) = &source else {
        unreachable!("a layer's source always names a file")
    };
    let file = file.clone();
    if matches!(layer, Layer::Repository) {
        // Someone else's text: a link, or anything but a directory and a
        // regular file, is refused before the lock is taken, as
        // `Config::load` refuses to read it.
        plain(&workspace.join(".fiber"), true)?;
        plain(&file, false)?;
    }
    Ok((file, source))
}

/// Why `set` refused `key`: the notice `keys::check` pushed, read back off
/// the table, since `keys` names no reason of its own. Only called with a
/// notice in hand, so a known key that is not repository-settable was
/// written to a repository, a global-only key was written to another layer,
/// a person-files key was written outside the person's own files, and any
/// other known key is repository-only.
fn refused_why(segments: &[String], source: &Source) -> &'static str {
    match keys::leaf(segments) {
        None => "this Fiber does not know it",
        Some(found) if !found.scope.repo() && matches!(source, Source::Repository(_)) => {
            "a repository may not set it"
        }
        Some(found) => match found.scope {
            keys::WriteScope::GlobalOnly => "only Fiber home's `config.json` may set it",
            keys::WriteScope::PersonFiles => {
                "only Fiber home's `config.json` or the project's `config.json` in Fiber home may set it"
            }
            keys::WriteScope::Any { .. } | keys::WriteScope::RepoOnly => {
                "only a repository's own file may set it"
            }
        },
    }
}

/// Parses `key` and checks one key's value against "Keys" for `source`,
/// returning the segments and the notices the check pushed. Shared by
/// [`set_global_if_unset`], [`replace_global`] and [`set`]:
/// only `set` turns a notice into a refusal, so an unknown key is still
/// written elsewhere.
fn checked(
    key: &str,
    value: Value,
    source: &Source,
) -> Result<(Vec<String>, Vec<Notice>), ConfigError> {
    let segments = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
    let mut candidate = Value::Object(Map::new());
    path::set(&mut candidate, &segments, value);
    let mut notices = Vec::new();
    if let Value::Object(map) = candidate {
        keys::check(map, source, &mut notices)?;
    }
    Ok((segments, notices))
}

/// Sets one key when the global `config.json` holds no value at `key`,
/// checked and written under one lock (`fiber login` writing
/// `providers."<name>".credential` with a provider's first label). `true`
/// when it wrote. A project layer's value does not count: only the global
/// file is looked at.
pub fn set_global_if_unset(home: &Path, key: &str, value: Value) -> Result<bool, ConfigError> {
    let file = global_config_file(home);
    let (segments, _) = checked(key, value.clone(), &Source::Global(file.clone()))?;
    update(&file, &segments, value, true)
}

/// Sets one key in the global `config.json` to `value`, or removes it for
/// `None`, under one lock and one atomic write, returning the value it
/// replaced (`fiber hub install` writing `hub.port`). A value is type-checked
/// as [`set_global_if_unset`] checks it. Removing a key the file does not hold
/// writes nothing. A failure before the rename leaves the file unchanged; a
/// failure after it, syncing the directory, returns `Err` with the new
/// content already in place.
pub fn replace_global(
    home: &Path,
    key: &str,
    value: Option<Value>,
) -> Result<Option<Value>, ConfigError> {
    let file = global_config_file(home);
    let segments = match &value {
        Some(value) => checked(key, value.clone(), &Source::Global(file.clone()))?.0,
        None => path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?,
    };
    let _lock = locked(&file)?;
    let mut root = read(&file)?.unwrap_or_else(|| Value::Object(Map::new()));
    // A concurrent write may have replaced the whole document with a
    // shape no layer may hold, such as `[]`, which `path::set` below
    // would silently destroy. Validate the locked read against "Keys"
    // on a clone, keeping the original root (with its unknown keys and
    // spelling) as the read-modify-write document.
    {
        let mut notices = Vec::new();
        crate::check_layer(root.clone(), &Source::Global(file.clone()), &mut notices)?;
    }
    let previous = match value {
        Some(value) => {
            let previous = path::get(&root, &segments).cloned();
            path::set(&mut root, &segments, value);
            previous
        }
        None => match path::remove(&mut root, &segments) {
            Some(previous) => Some(previous),
            None => return Ok(None),
        },
    };
    write_root(&file, &root)?;
    Ok(previous)
}

/// Clears a global key only when its value equals `expected`, comparing and
/// writing under the same lock. An absent or different value writes nothing.
pub fn clear_global_if_equals(
    home: &Path,
    key: &str,
    expected: &Value,
) -> Result<bool, ConfigError> {
    let file = global_config_file(home);
    let segments = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
    let _lock = locked(&file)?;
    let Some(mut root) = read(&file)? else {
        return Ok(false);
    };
    if path::get(&root, &segments) != Some(expected) {
        return Ok(false);
    }
    let mut notices = Vec::new();
    crate::check_layer(root.clone(), &Source::Global(file.clone()), &mut notices)?;
    path::remove(&mut root, &segments);
    write_root(&file, &root)?;
    Ok(true)
}

/// Inserts one named entry under `key` in a layer's file (`fiber mcp add`
/// writing `mcp.servers`). The value is type-checked as [`set`] checks it,
/// so on any `Err` the file is unchanged. `true` when it wrote; `false`
/// when `name` was already there, the file untouched. The presence check
/// runs under the file's lock, so a lost race is the same refusal.
pub fn insert_entry(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: Layer,
    key: &str,
    name: &str,
    value: Value,
) -> Result<bool, ConfigError> {
    let (file, source) = layer_file(home, workspace, project, layer)?;
    let mut segments = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
    segments.push(name.to_owned());
    // Checked from the segments, never through a displayed key: a name
    // holding a dot is one segment, not two.
    let mut candidate = Value::Object(Map::new());
    path::set(&mut candidate, &segments, value.clone());
    let mut notices = Vec::new();
    if let Value::Object(map) = candidate {
        keys::check(map, &source, &mut notices)?;
    }
    if !notices.is_empty() {
        return Err(ConfigError::Refused {
            key: path::display(&segments),
            file,
            why: refused_why(&segments, &source),
        });
    }
    let _lock = locked(&file)?;
    let mut root = read(&file)?.unwrap_or_else(|| Value::Object(Map::new()));
    if path::get(&root, &segments).is_some() {
        return Ok(false);
    }
    path::set(&mut root, &segments, value);
    write_root(&file, &root)?;
    Ok(true)
}

/// Removes one named entry from under `key` in a layer's file (`fiber mcp
/// remove`). `true` when it removed one. `false` when the name or the file
/// is absent, creating and changing no file. Removing the last entry
/// removes each object on the way that it empties.
pub fn remove_entry(
    home: &Path,
    workspace: &Path,
    project: &ProjectKey,
    layer: Layer,
    key: &str,
    name: &str,
) -> Result<bool, ConfigError> {
    let (file, _) = layer_file(home, workspace, project, layer)?;
    let mut segments = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
    let parents = segments.len();
    segments.push(name.to_owned());
    let _lock = locked(&file)?;
    let Some(mut root) = read(&file)? else {
        return Ok(false);
    };
    if path::remove(&mut root, &segments).is_none() {
        return Ok(false);
    }
    // Each emptied object on the key's way goes with its last entry.
    for len in (1..=parents).rev() {
        let Some(prefix) = segments.get(..len) else {
            continue;
        };
        if matches!(path::get(&root, prefix), Some(Value::Object(map)) if map.is_empty()) {
            path::remove(&mut root, prefix);
        }
    }
    write_root(&file, &root)?;
    Ok(true)
}

/// The value at `key` in Fiber home's `config.json` alone: no other layer
/// and no default is consulted. `None` when the file or the key is absent.
pub fn get_global(home: &Path, key: &str) -> Result<Option<Value>, ConfigError> {
    let segments = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
    Ok(read(&global_config_file(home))?.and_then(|root| path::get(&root, &segments).cloned()))
}

/// Deletes an extension's settings in the global and every per-project layer
/// (`fiber extension remove`).
pub fn remove_extension_settings(home: &Path, extension: &str) -> Result<(), ConfigError> {
    let mut dirs = vec![home.to_path_buf()];
    match fs::read_dir(home.join("projects")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|source| ConfigError::Io {
                    file: home.join("projects"),
                    source,
                })?;
                dirs.push(entry.path());
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ConfigError::Io {
                file: home.join("projects"),
                source,
            });
        }
    }
    for dir in dirs {
        let file = settings_file(&dir, extension);
        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(source) => return Err(ConfigError::Io { file, source }),
        }
    }
    Ok(())
}

/// `config/<extension>.json` under a layer's directory, `<extension>`
/// named as in `extensions/` (`docs/state.md`, "What each part holds").
pub fn settings_file(dir: &Path, extension: &str) -> PathBuf {
    dir.join("config")
        .join(format!("{}.json", crate::names::dir_name(extension)))
}

/// Reads `file` under its lock, sets one key and writes the whole file back,
/// keys sorted with a 2-space indent. With `only_if_unset`, a file that
/// already holds a value at `key` is left alone. `true` when it wrote.
pub(crate) fn update(
    file: &Path,
    key: &[String],
    value: Value,
    only_if_unset: bool,
) -> Result<bool, ConfigError> {
    let _lock = locked(file)?;
    let mut root = read(file)?.unwrap_or_else(|| Value::Object(Map::new()));
    if only_if_unset && path::get(&root, key).is_some() {
        return Ok(false);
    }
    path::set(&mut root, key, value);
    write_root(file, &root)?;
    Ok(true)
}

/// Writes `root` over `file`, keys sorted with a 2-space indent, for a
/// caller holding the file's lock.
fn write_root(file: &Path, root: &Value) -> Result<(), ConfigError> {
    let mut text = serde_json::to_string_pretty(root).map_err(|e| ConfigError::Io {
        file: file.to_path_buf(),
        source: e.into(),
    })?;
    text.push('\n');
    write_atomic(file, text.as_bytes(), 0o666)
}

#[cfg(test)]
#[path = "write_lock_tests.rs"]
mod lock_tests;

pub(crate) mod atomic;
pub(crate) mod lines;
mod notes;

mod entries;

pub use atomic::{atomic_error, write_atomic};
pub use entries::update_global_entries;
pub use notes::{
    FileOutcome, FileSaved, NotesSaved, NotesWrite, ReadOnlyEntryProblem, ReadOnlyFlags, overlaps,
    read_only_keys, save_reviewer_notes, validate_read_only_entry, version,
};

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
