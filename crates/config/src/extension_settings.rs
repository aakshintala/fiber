//! One extension's settings (`docs/configuration.md`, "Extension settings"):
//! the `config/<extension>.json` files in each layer, merged as they were
//! when this configuration was loaded, plus its own writes since.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::events::Notice;
use serde_json::{Map, Value};

use crate::error::ConfigError;
use crate::home::ProjectKey;
use crate::home::parse;
use crate::names::full_name;
use crate::write::{self, Scope};
use crate::{Source, path, top_level};

/// One extension's settings files, by layer.
#[derive(Clone)]
pub struct ExtensionSettings {
    home: PathBuf,
    workspace: PathBuf,
    project: ProjectKey,
    /// `-c extensions."<name>".settings.<key>=value`, by extension.
    run_settings: Map<String, Value>,
    /// The bytes of every `config/<extension>.json` in each layer, by path.
    files: BTreeMap<PathBuf, Vec<u8>>,
    /// A replacement for one extension's repository-layer contribution in
    /// this in-memory clone: the session's authorized repository entries.
    /// It never persists and never outranks project or run.
    overlay: BTreeMap<String, Value>,
}

impl ExtensionSettings {
    pub(crate) fn new(
        home: PathBuf,
        workspace: PathBuf,
        project: ProjectKey,
        run_settings: Map<String, Value>,
        files: BTreeMap<PathBuf, Vec<u8>>,
    ) -> Self {
        Self {
            home,
            workspace,
            project,
            run_settings,
            files,
            overlay: BTreeMap::new(),
        }
    }

    /// Replaces this extension's repository-layer contribution in this
    /// in-memory clone with `repository`: the session's authorized entries
    /// for it. Global still beats nothing below it, and project and run
    /// still beat the overlay, so an approved entry merges key by key in
    /// normal layer order. It never touches another extension or the files
    /// on disk.
    pub fn set_overlay(&mut self, extension: &str, repository: Value) {
        self.overlay.insert(full_name(extension), repository);
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
    pub fn get(
        &self,
        extension: &str,
        repo_settings: &[&str],
        key: &str,
    ) -> Result<Option<Value>, ConfigError> {
        let key = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
        // `merged` reads its name as either spelling.
        let (merged, _) = self.merged(extension, repo_settings)?;
        Ok(path::get(&merged, &key).cloned())
    }

    /// An extension's settings, merged across its layers as they were when
    /// this configuration was loaded, plus its own writes since
    /// (`docs/configuration.md`, "Extension settings"). The repository's file
    /// may set only the keys the extension's manifest lists under
    /// `repo_settings`; any other key there is a notice and is ignored.
    pub fn merged(
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
            if repo && let Some(overlay) = self.overlay.get(extension) {
                // The authorized replacement still reports what the raw
                // file holds that the manifest forbids, so a disallowed
                // key keeps its notice while its value stays withheld.
                if let Some(bytes) = self.files.get(&file) {
                    let Value::Object(map) = parse(&file, bytes)? else {
                        return Err(top_level(&file.display().to_string()));
                    };
                    for key in map.keys() {
                        if !repo_settings.contains(&key.as_str()) {
                            notices.push(Notice {
                                code: ErrorCode::ConfigKeyIgnored,
                                message: format!(
                                    "{}: ignored `{key}`, which the extension does not list under repo_settings.",
                                    file.display()
                                ),
                                extension: Some(extension.into()),
                            });
                        }
                    }
                }
                path::merge(&mut merged, overlay);
                continue;
            }
            let Some(bytes) = self.files.get(&file) else {
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
    pub fn set(
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
        let mut cached = match self.files.get(&file) {
            Some(bytes) => parse(&file, bytes)?,
            None => Value::Object(Map::new()),
        };
        path::set(&mut cached, &key, value.clone());
        // The file on disk may hold other sessions' writes too; this session
        // sees only its own until its next load.
        write::update(&file, &key, value, false)?;
        self.files.insert(file, cached.to_string().into_bytes());
        Ok(())
    }
}

/// Shows where the settings came from, never a value: a setting may be
/// something the person would not print.
impl fmt::Debug for ExtensionSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtensionSettings")
            .field("home", &self.home)
            .field("workspace", &self.workspace)
            .field("project", &self.project)
            .field("run_settings", &self.run_settings.keys())
            .field("files", &self.files.keys())
            .field("overlay", &self.overlay.keys())
            .finish_non_exhaustive()
    }
}

/// Splits an extension-settings key `extensions.<name>.settings.<rest>`
/// into the extension's full name and the relative segments below its
/// `settings`, the empty rest included. `None` for every other shape and
/// for unparseable keys, which keep their existing errors.
pub fn settings_key(key: &str) -> Option<(String, Vec<String>)> {
    match path::parse(key)?.as_slice() {
        [area, name, settings, rest @ ..] if area == "extensions" && settings == "settings" => {
            Some((full_name(name), rest.to_vec()))
        }
        _ => None,
    }
}

/// The layered read of one settings key: the value with its source when
/// some layer holds it, beside the repository notices the read ignored.
type LayeredGet = (Option<(Value, Source)>, Vec<Notice>);

impl ExtensionSettings {
    /// The effective value of `key` in `extension`'s settings, the layer
    /// it came from, and the repository notices the read ignored:
    /// `-c` run settings, then the project file, then the repository
    /// file, then the global file (`docs/configuration.md`, "Extension
    /// settings"). The value reads through [`Self::merged`], so a higher
    /// scalar over a lower object hides it there already; only the source
    /// walks the layers highest-first. Corrupt JSON and non-object files
    /// are an error. `None` when no layer holds the key.
    pub fn get_layered(
        &self,
        extension: &str,
        repo_settings: &[&str],
        key: &str,
    ) -> Result<LayeredGet, ConfigError> {
        let key = path::parse(key).ok_or_else(|| ConfigError::Override { arg: key.into() })?;
        let (merged, notices) = self.merged(extension, repo_settings)?;
        let Some(value) = path::get(&merged, &key).cloned() else {
            return Ok((None, notices));
        };
        let extension = &full_name(extension);
        let global_file = write::settings_file(&self.home, extension);
        let repository_file = write::settings_file(&self.workspace.join(".fiber"), extension);
        let project_file = write::settings_file(
            &self.home.join("projects").join(self.project.as_str()),
            extension,
        );
        let repository_subtree = if let Some(overlay) = self.overlay.get(extension) {
            // The overlay already holds only authorized entries, so a
            // withheld repository value never names the repository layer.
            // A corrupt raw file still errors through `merged` above.
            Some(overlay.clone())
        } else {
            repository_subtree(&self.files, &repository_file, repo_settings)?
        };
        let mut layers: Vec<(Source, Option<Value>)> = vec![
            (
                Source::Global(global_file.clone()),
                file_subtree(&self.files, &global_file)?,
            ),
            (
                Source::Repository(repository_file.clone()),
                repository_subtree,
            ),
            (
                Source::Project(project_file.clone()),
                file_subtree(&self.files, &project_file)?,
            ),
        ];
        if let Some(run) = self.run_settings.get(extension) {
            layers.push((Source::Run, Some(run.clone())));
        }
        // Highest first: the first layer holding the key names the
        // source, as `Config::get_segments` names it.
        for (source, subtree) in layers.iter().rev() {
            if let Some(subtree) = subtree
                && path::get(subtree, &key).is_some()
            {
                return Ok((Some((value, source.clone())), notices));
            }
        }
        Ok((None, notices))
    }
}

/// The extension's settings the snapshot holds at `file`: `None` when the
/// layer holds no file. Corrupt JSON is an error; a file that reads as
/// valid JSON of a non-object type is the `top_level` shape error, as
/// [`ExtensionSettings::merged`] reports it.
fn file_subtree(
    files: &BTreeMap<PathBuf, Vec<u8>>,
    file: &Path,
) -> Result<Option<Value>, ConfigError> {
    let Some(bytes) = files.get(file) else {
        return Ok(None);
    };
    match parse(file, bytes)? {
        Value::Object(map) => Ok(Some(Value::Object(map))),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Err(top_level(&file.display().to_string()))
        }
    }
}

/// The repository layer's contribution: the file's object kept to the
/// top-level keys `repo_settings` lists, as [`ExtensionSettings::merged`]
/// keeps it. Any other key there is dropped here; the session collects
/// the notice once at load.
fn repository_subtree(
    files: &BTreeMap<PathBuf, Vec<u8>>,
    file: &Path,
    repo_settings: &[&str],
) -> Result<Option<Value>, ConfigError> {
    let Some(mut kept) = file_subtree(files, file)? else {
        return Ok(None);
    };
    if let Value::Object(map) = &mut kept {
        map.retain(|key, _| repo_settings.contains(&key.as_str()));
    }
    Ok(Some(kept))
}
