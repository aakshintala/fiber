//! The terminal's configuration seam (`docs/tui.md`, "Swapped views"):
//! `/settings` reads every key through `config` and writes through the
//! same path as `fiber config set`, for the project of the workspace the
//! view is about, and lists the themes in Fiber home's `themes/` and each
//! installed extension's. The
//! terminal loads configuration with no `-c`, so no row comes from one.

mod install;
mod login;
mod logout;
mod models;
mod remote;
mod skills;
mod tools;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use config::{Config, SettingValue, Source, Sources};
use contract::shapes::Failure;
use tui::{
    ConfigureError, HostGap, HostSaved, KeyEdit, Layer, LoginTarget, Revoked, RuleRow, RulesScope,
    RulesSection, Saved, SettingRow, Shown, SkillsDisabled, Stored, SwitchScope, ToolGroup,
    ToolSwitches, WriteScope,
};

use crate::switch::Reads;

/// The seam over Fiber home.
pub(crate) struct Seam {
    home: PathBuf,
    origin: Arc<extensions::Origin>,
    /// The process's shared credential reads: a `models` refresh runs
    /// its credential commands through these, so one cancel reaches
    /// them all.
    reads: Arc<Reads>,
    /// The per-path locks a `models` refresh holds, the same lock the
    /// local picker read takes.
    locks: Arc<::tools::PathLocks>,
    /// Each workspace's project key, resolved once per path for the
    /// seam's life.
    projects: Mutex<HashMap<PathBuf, config::ProjectKey>>,
}

impl Seam {
    /// The seam over Fiber home `home`.
    pub(crate) fn new(home: PathBuf) -> Self {
        Self {
            home,
            origin: Arc::new(extensions::Origin::github()),
            reads: Arc::new(Reads::default()),
            locks: Arc::new(::tools::PathLocks::new()),
            projects: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn with_origin(home: PathBuf, origin: Arc<extensions::Origin>) -> Self {
        Self {
            origin,
            ..Self::new(home)
        }
    }

    /// The workspace's project key, resolved once per path and cached
    /// for the seam's life. A failed resolve is retried: only keys are
    /// cached, never failures. The key goes stale when the workspace's
    /// repository changes (a `git init`, a reused path, a recreated
    /// worktree) until the hub restarts, while `set` stays uncached and
    /// writes under the fresh key.
    fn project_key(&self, workspace: &Path) -> Result<config::ProjectKey, Failure> {
        if let Some(key) = support::lock(&self.projects).get(workspace).cloned() {
            return Ok(key);
        }
        let (_, key) = ::cli::project_of(&self.home, workspace)?;
        Ok(support::lock(&self.projects)
            .entry(workspace.to_path_buf())
            .or_insert(key)
            .clone())
    }

    /// The configuration `workspace`'s project and repository give.
    fn load(&self, workspace: &Path) -> Result<Config, ConfigureError> {
        let project = self.project_key(workspace).map_err(from_failure)?;
        Config::load(Sources {
            home: self.home.clone(),
            workspace: workspace.to_path_buf(),
            project,
            overrides: Vec::new(),
        })
        .map_err(from_config)
    }
}

/// A failure as the views show it.
fn from_failure(failure: Failure) -> ConfigureError {
    ConfigureError {
        code: failure.code,
        message: failure.message,
    }
}

/// A configuration failure as the views show it.
fn from_config(error: config::ConfigError) -> ConfigureError {
    ConfigureError {
        code: error.code(),
        message: error.to_string(),
    }
}

/// The terminal's scope for `config`'s.
fn scope_to(scope: RulesScope) -> config::RulesScope {
    match scope {
        RulesScope::Global => config::RulesScope::Global,
        RulesScope::Project => config::RulesScope::Project,
    }
}

/// One rules file's rows for the view.
fn section(listing: config::RulesListing) -> RulesSection {
    RulesSection {
        file: listing.file,
        rows: listing
            .lines
            .map(|lines| {
                lines
                    .into_iter()
                    .map(|line| RuleRow {
                        line: line.line,
                        text: line.text,
                        rule: line.rule,
                    })
                    .collect()
            })
            .map_err(|error| error.to_string()),
    }
}

/// One notes layer's file, text and version for the view.
fn notes_file(file: config::NotesFile) -> tui::NotesFile {
    tui::NotesFile {
        file: file.file,
        text: file.text,
        version: file.version,
    }
}

/// A layer's name, as `/settings` shows it, and its file when it has one.
fn layer_of(source: &Source) -> (&'static str, Option<PathBuf>) {
    match source {
        Source::Default => ("default", None),
        Source::Global(file) => ("global", Some(file.clone())),
        Source::Repository(file) => ("repository", Some(file.clone())),
        Source::Project(file) => ("project", Some(file.clone())),
        Source::Run => ("-c", None),
    }
}

/// The terminal's layer for `config`'s.
fn layer_to(layer: Layer) -> config::Layer {
    match layer {
        Layer::Global => config::Layer::Global,
        Layer::Project => config::Layer::Project,
        Layer::Repository => config::Layer::Repository,
    }
}

/// One key's row.
fn row(config: &Config, info: config::SettingInfo) -> SettingRow {
    let scope = match info.scope {
        config::WriteScope::Any { repo } => WriteScope::Any { repo },
        config::WriteScope::GlobalOnly => WriteScope::GlobalOnly,
        config::WriteScope::RepoOnly => WriteScope::RepoOnly,
        config::WriteScope::PersonFiles => WriteScope::PersonFiles,
    };
    let (value, layer, file) = match info.value {
        SettingValue::Unset => (Shown::Unset, "default".to_owned(), None),
        SettingValue::Value(value, source) => {
            let (layer, file) = layer_of(&source);
            (Shown::Value(value.to_string()), layer.to_owned(), file)
        }
        SettingValue::Redacted(text, source) => {
            let (layer, file) = layer_of(&source);
            (Shown::Redacted(text), layer.to_owned(), file)
        }
        SettingValue::Union(names) => {
            let own = [Layer::Global, Layer::Project, Layer::Repository]
                .into_iter()
                .filter_map(|layer| {
                    config
                        .in_layer(&info.key, layer_to(layer))
                        .map(|list| (layer, list.to_string()))
                })
                .collect::<Vec<_>>();
            // Each layer's own list decides whether it shows, not the
            // deduplicated names: two layers listing the same name both
            // show (`docs/tui.md`, "Swapped views").
            let layers = own
                .iter()
                .map(|(layer, _)| match layer {
                    Layer::Global => "global",
                    Layer::Project => "project",
                    Layer::Repository => "repository",
                })
                .collect::<Vec<_>>()
                .join(" + ");
            let names = names
                .into_iter()
                .map(|(name, source)| {
                    let (layer, _) = layer_of(&source);
                    (name, layer.to_owned())
                })
                .collect();
            (Shown::Union { names, own }, layers, None)
        }
    };
    SettingRow {
        key: info.key,
        value,
        layer,
        file,
        scope,
    }
}

impl tui::Configure for Seam {
    fn import_pending(&self) -> bool {
        config::import_pending(&self.home)
    }

    fn install_provider(
        &self,
        workspace: &Path,
        provider: &str,
        clock: Arc<dyn contract::clock::Clock>,
    ) -> Result<tui::ProviderInstall, ConfigureError> {
        install::provider(self, workspace, provider, clock)
    }

    fn settings(&self, workspace: &Path) -> Result<Vec<SettingRow>, ConfigureError> {
        let config = self.load(workspace)?;
        Ok(config
            .settings()
            .into_iter()
            .map(|info| row(&config, info))
            .collect())
    }

    fn set(
        &self,
        workspace: &Path,
        layer: Layer,
        key: &str,
        text: &str,
    ) -> Result<Saved, ConfigureError> {
        let (warnings, file) = ::cli::config_set_text(
            &self.home,
            workspace,
            layer_to(layer),
            key,
            text,
            &crate::clock::System,
        )
        .map_err(from_failure)?;
        Ok(Saved { file, warnings })
    }

    fn global_file(&self) -> PathBuf {
        config::global_config_file(&self.home)
    }

    fn rules(&self, workspace: &Path) -> Result<(RulesSection, RulesSection), ConfigureError> {
        let project = self.project_key(workspace).map_err(from_failure)?;
        let (global, project) = config::list_rules(&self.home, &project);
        Ok((section(global), section(project)))
    }

    fn revoke(
        &self,
        workspace: &Path,
        scope: RulesScope,
        line: usize,
        text: &str,
    ) -> Result<Revoked, ConfigureError> {
        let project = self.project_key(workspace).map_err(from_failure)?;
        let removed = config::remove_rule(&self.home, &project, scope_to(scope), line, text)
            .map_err(from_config)?;
        Ok(if removed {
            Revoked::Removed
        } else {
            Revoked::Stale
        })
    }

    // `/login`.

    fn login_targets(&self) -> Result<Vec<LoginTarget>, ConfigureError> {
        login::targets(&self.home)
    }

    fn store_key(
        &self,
        name: &str,
        label: Option<&str>,
        key: contract::Secret,
    ) -> Result<Stored, ConfigureError> {
        login::store(&self.home, name, label, key)
    }

    fn browser_login(
        &self,
        name: &str,
        shown: std::sync::Arc<dyn tui::LoginShow>,
        clock: std::sync::Arc<dyn contract::clock::Clock>,
    ) -> std::sync::Arc<dyn tui::BrowserLogin> {
        login::browser_login(
            &self.home,
            name,
            shown,
            clock,
            extensions::LoginMethod::Browser,
        )
    }

    fn reviewer_models(
        &self,
        name: &str,
        clock: std::sync::Arc<dyn contract::clock::Clock>,
    ) -> Result<tui::ReviewerModels, ConfigureError> {
        login::reviewer_models(&self.home, name, clock)
    }

    fn set_reviewer(&self, name: &str, id: &str) -> Result<(), ConfigureError> {
        login::set_reviewer(&self.home, name, id)
    }

    fn gaps(&self, name: &str) -> Result<Vec<HostGap>, ConfigureError> {
        login::gaps(&self.home, name)
    }

    fn save_host(&self, name: &str, key: &str, value: &str) -> Result<HostSaved, ConfigureError> {
        login::save_host(&self.home, name, key, value)
    }

    fn logout_rows(&self) -> Result<Vec<tui::LogoutRow>, ConfigureError> {
        let config = self.load(&self.home)?;
        logout::rows(&self.home, &config)
    }

    fn delete_stored(&self, name: &str, label: Option<String>) -> Result<String, ConfigureError> {
        logout::delete(&self.home, name, label)
    }

    fn reviewer_notes(&self, workspace: &Path) -> Result<tui::ReviewerNotes, ConfigureError> {
        let project = self.project_key(workspace).map_err(from_failure)?;
        let notes = config::reviewer_notes(&self.home, &project).map_err(from_config)?;
        Ok(tui::ReviewerNotes {
            global: notes_file(notes.global),
            project: notes_file(notes.project),
        })
    }

    fn save_reviewer_notes(
        &self,
        workspace: &Path,
        global: Option<tui::NotesWrite>,
        project: Option<tui::NotesWrite>,
    ) -> Result<tui::NotesSaved, ConfigureError> {
        let key = self.project_key(workspace).map_err(from_failure)?;
        let saved = config::save_reviewer_notes(
            &self.home,
            &key,
            global.map(|write| config::NotesWrite {
                version: write.version,
                text: write.text,
            }),
            project.map(|write| config::NotesWrite {
                version: write.version,
                text: write.text,
            }),
        )
        .map_err(from_config)?;
        Ok(match saved {
            config::NotesSaved::Stale => tui::NotesSaved::Stale,
            config::NotesSaved::Written(files) => tui::NotesSaved::Written(
                files
                    .into_iter()
                    .map(|saved| tui::FileSaved {
                        file: saved.file,
                        outcome: match saved.outcome {
                            config::FileOutcome::Saved => tui::FileOutcome::Saved,
                            config::FileOutcome::SavedUnsynced(error) => {
                                tui::FileOutcome::SavedUnsynced(from_config(error))
                            }
                            config::FileOutcome::NotSaved(error) => {
                                tui::FileOutcome::NotSaved(from_config(error))
                            }
                        },
                    })
                    .collect(),
            ),
        })
    }

    fn themes(&self, workspace: &Path) -> Vec<String> {
        match self.load(workspace) {
            Ok(config) => crate::theme_setting::names(&self.home, &|name| {
                extensions::is_enabled(&config, name)
            }),
            // The other reads fail the workspace: listing cannot, so
            // without a project no extension's `enabled` is known and
            // only Fiber home's themes list.
            Err(_) => crate::theme_setting::names(&self.home, &|_: &str| false),
        }
    }

    fn theme(&self, workspace: &Path, name: &str) -> tui::ThemeSetting {
        match self.load(workspace) {
            Ok(config) => crate::theme_setting::named(
                &self.home,
                Some(name),
                &|path| std::fs::read_to_string(path),
                &|extension| extensions::is_enabled(&config, extension),
            ),
            // As with `themes` above: without a project the read is
            // Fiber home only.
            Err(_) => crate::theme_setting::named(
                &self.home,
                Some(name),
                &|path| std::fs::read_to_string(path),
                &|_: &str| false,
            ),
        }
    }

    fn tool_switches(&self, workspace: &Path) -> Result<Vec<ToolSwitches>, ConfigureError> {
        self.read_switches(workspace)
    }

    fn switch_tool(
        &self,
        workspace: &Path,
        group: &ToolGroup,
        tool: &str,
        scope: SwitchScope,
        on: bool,
    ) -> Result<(), ConfigureError> {
        self.write_switch(workspace, group, tool, scope, on)
    }

    fn skills_disabled(&self, workspace: &Path) -> Result<SkillsDisabled, ConfigureError> {
        self.read_skills_disabled(workspace)
    }

    fn switch_skill(
        &self,
        workspace: &Path,
        name: &str,
        scope: SwitchScope,
        on: bool,
    ) -> Result<(), ConfigureError> {
        self.write_skill_switch(workspace, name, scope, on)
    }

    fn skill_text(&self, path: &Path) -> Result<String, ConfigureError> {
        self.read_skill_text(path)
    }

    fn save_keys(&self, edits: &[KeyEdit]) -> Result<(), ConfigureError> {
        let entries: Vec<(String, Option<serde_json::Value>)> = edits
            .iter()
            .map(|edit| {
                let keys = edit.keys.as_ref().map(|names| {
                    serde_json::Value::Array(
                        names
                            .iter()
                            .map(|name| serde_json::Value::String(name.clone()))
                            .collect(),
                    )
                });
                (edit.id.clone(), keys)
            })
            .collect();
        config::update_global_entries(&self.home, "keys", &entries).map_err(from_config)
    }
}

#[cfg(test)]
#[path = "configure_tests.rs"]
mod tests;
