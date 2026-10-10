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
use crate::{path, top_level};

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
        }
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
            .finish_non_exhaustive()
    }
}
