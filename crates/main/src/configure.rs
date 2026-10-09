//! The terminal's configuration seam (`docs/tui.md`, "Swapped views"):
//! `/settings` reads every key through `config` and writes through the
//! same path as `fiber config set`, for the project of the workspace the
//! view is about, and lists the themes in Fiber home's `themes/`. The
//! terminal loads configuration with no `-c`, so no row comes from one.

mod login;
mod skills;
mod tools;

use std::path::{Path, PathBuf};

use config::{Config, SettingValue, Source, Sources};
use contract::shapes::Failure;
use tui::{
    ConfigureError, KeyEdit, Layer, LoginTarget, Revoked, RuleRow, RulesScope, RulesSection, Saved,
    SettingRow, Shown, SkillsDisabled, Stored, SwitchScope, ToolGroup, ToolSwitches, WriteScope,
};

/// The seam over Fiber home.
pub(crate) struct Seam {
    home: PathBuf,
}

impl Seam {
    /// The seam over Fiber home `home`.
    pub(crate) fn new(home: PathBuf) -> Self {
        Self { home }
    }

    /// The configuration `workspace`'s project and repository give.
    fn load(&self, workspace: &Path) -> Result<Config, ConfigureError> {
        let (_, project) = ::cli::project_of(&self.home, workspace).map_err(from_failure)?;
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
        let warnings = ::cli::config_set_text(&self.home, workspace, layer_to(layer), key, text)
            .map_err(from_failure)?;
        let file = match layer {
            Layer::Global => self.global_file(),
            Layer::Project => {
                let (_, project) =
                    ::cli::project_of(&self.home, workspace).map_err(from_failure)?;
                self.home
                    .join("projects")
                    .join(project.as_str())
                    .join("config.json")
            }
            Layer::Repository => workspace.join(".fiber").join("config.json"),
        };
        Ok(Saved { file, warnings })
    }

    fn global_file(&self) -> PathBuf {
        self.home.join("config.json")
    }

    fn rules(&self, workspace: &Path) -> Result<(RulesSection, RulesSection), ConfigureError> {
        let (_, project) = ::cli::project_of(&self.home, workspace).map_err(from_failure)?;
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
        let (_, project) = ::cli::project_of(&self.home, workspace).map_err(from_failure)?;
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

    fn themes(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.home.join("themes")) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let stem = name.strip_suffix(".json")?;
                (!stem.is_empty() && !stem.starts_with('.')).then(|| stem.to_owned())
            })
            .collect();
        names.sort();
        names
    }

    fn theme(&self, name: &str) -> tui::ThemeSetting {
        crate::theme_setting::named(&self.home, Some(name), &|path| {
            std::fs::read_to_string(path)
        })
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
