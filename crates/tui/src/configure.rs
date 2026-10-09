//! The seam the configuration views read and write through
//! (`docs/tui.md`, "Swapped views"). The terminal never reads
//! configuration itself (`docs/architecture.md`, "The call rules"): `main`
//! implements [`Configure`] and passes it in [`crate::Launch`]. Every call
//! is a few small file reads or one locked write, made only on a person's
//! action, so it runs on the loop's thread.

use std::fmt;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::Secret;

use crate::ThemeSetting;

/// Which configuration file a write goes to (`docs/configuration.md`,
/// "When Fiber writes").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// `config.json` at the top of Fiber home.
    Global,
    /// The project's `config.json` in Fiber home.
    Project,
    /// The workspace's `.fiber/config.json`.
    Repository,
}

/// Which files a key may be written to ("What a repository may set").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteScope {
    /// The global and the project's file, and the repository's when
    /// `repo`.
    Any {
        /// Whether a repository may set it.
        repo: bool,
    },
    /// Only the global file.
    GlobalOnly,
    /// Only the repository's file.
    RepoOnly,
    /// Only the person's own files: the global and the project's.
    PersonFiles,
}

/// A key's effective value as `/settings` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown {
    /// No layer sets it and it has no default.
    Unset,
    /// The value as compact JSON.
    Value(String),
    /// A list whose layers all apply (`skills.disabled`).
    Union {
        /// Each name with the layer listing it.
        names: Vec<(String, String)>,
        /// Each layer's own list as compact JSON, for the layers that set
        /// one: what a write to that layer starts from.
        own: Vec<(Layer, String)>,
    },
    /// A value that may hold a secret, described without it.
    Redacted(String),
}

/// One configuration key's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    /// The dotted key.
    pub key: String,
    /// Its effective value.
    pub value: Shown,
    /// The layer it comes from: `default`, `global`, `repository`,
    /// `project` or `-c`, or the layers of a union.
    pub layer: String,
    /// The file of that layer, when it is one of the person's or the
    /// repository's files.
    pub file: Option<PathBuf>,
    /// The files it may be written to.
    pub scope: WriteScope,
}

/// What a write did: the file written and the warning lines
/// `fiber config set` would print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The file written.
    pub file: PathBuf,
    /// Warnings, one line each.
    pub warnings: Vec<String>,
}

/// Which rules file (`docs/configuration.md`, "Standing rules").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesScope {
    /// `rules` at the top of Fiber home.
    Global,
    /// `projects/<key>/rules` in Fiber home.
    Project,
}

/// One line of a rules file: its physical number from 1, its text, its
/// rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRow {
    /// The line's physical number, counted from 1 over every line.
    pub line: usize,
    /// The line's text, without its line ending.
    pub text: String,
    /// The rule the line parses to.
    pub rule: contract::Rule,
}

/// One rules file: its path, and its rules or why it cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesSection {
    /// The file listed.
    pub file: PathBuf,
    /// The file's rules, in order; a missing file holds none.
    pub rows: Result<Vec<RuleRow>, String>,
}

/// How a `/login` row logs in (`docs/tui.md`, "Logging in").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginKind {
    /// A provider whose key goes in the hidden field.
    Key,
    /// A provider that logs in through the browser.
    Browser,
    /// A secret an installed extension declares.
    Secret,
}

/// One `/login` row: a provider or a declared secret, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginTarget {
    /// The provider or secret's name.
    pub name: String,
    /// How the row logs in.
    pub kind: LoginKind,
}

/// What a login stored: the file under Fiber home, and whether it replaced one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The file under Fiber home, such as `credentials/acme/default`.
    pub path: String,
    /// Whether it replaced a stored secret.
    pub replaced: bool,
}

/// What a revoke did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revoked {
    /// The line was deleted.
    Removed,
    /// The line had moved, changed or gone: nothing was written.
    Stale,
}

/// Why a read or a write failed: the failure's code and its message,
/// which the view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigureError {
    /// The failure's code.
    pub code: ErrorCode,
    /// What the view shows.
    pub message: String,
}

impl fmt::Display for ConfigureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// An MCP server or an extension, whose tools a `/tools` switch turns on
/// or off. Extensions sort before servers, each by name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolGroup {
    /// The extension's full name.
    Extension(String),
    /// The server's name.
    Mcp(String),
}

/// Which file a switch writes: the project's or the global `config.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchScope {
    /// The project's `config.json` in Fiber home.
    Project,
    /// The global `config.json`.
    Everywhere,
}

/// Each person file's own `skills.disabled` (`docs/configuration.md`,
/// "Layers"): the global list and the project's list both apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillsDisabled {
    /// The project's `config.json` list.
    pub project: Vec<String>,
    /// The global `config.json` list.
    pub everywhere: Vec<String>,
}

/// A layer's `tools.enabled` and `tools.disabled`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolLists {
    /// The names to declare, when the layer sets a list.
    pub enabled: Option<Vec<String>>,
    /// The names to leave out.
    pub disabled: Vec<String>,
}

/// One group's lists: the effective ones for the workspace, and the global file's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSwitches {
    /// The server or extension.
    pub group: ToolGroup,
    /// The lists in force for the workspace.
    pub project: ToolLists,
    /// The global file's own lists.
    pub everywhere: ToolLists,
}

/// Reads and writes configuration for the views, about the session on
/// screen's workspace. Its methods are grouped by the view that calls
/// them.
pub trait Configure: Send + Sync {
    // `/settings`.

    /// Every configuration key with its effective value and layer, for
    /// `workspace`'s project and repository.
    fn settings(&self, workspace: &Path) -> Result<Vec<SettingRow>, ConfigureError>;

    /// Writes `key` in `layer`'s file from `text`, through the same path
    /// as `fiber config set`.
    fn set(
        &self,
        workspace: &Path,
        layer: Layer,
        key: &str,
        text: &str,
    ) -> Result<Saved, ConfigureError>;

    /// Fiber home's `config.json`.
    fn global_file(&self) -> PathBuf;

    // `/rules`.

    /// The global and the project's rules files, for `workspace`'s
    /// project.
    fn rules(&self, workspace: &Path) -> Result<(RulesSection, RulesSection), ConfigureError>;

    /// Deletes line `line` of `scope`'s file when it still reads `text`.
    fn revoke(
        &self,
        workspace: &Path,
        scope: RulesScope,
        line: usize,
        text: &str,
    ) -> Result<Revoked, ConfigureError>;

    // `/login`.

    /// The installed providers by name, then the secrets installed
    /// extensions declare, as `fiber login`'s menu lists them.
    fn login_targets(&self) -> Result<Vec<LoginTarget>, ConfigureError>;

    /// Stores `key` for provider or secret `name`, under `label` for a
    /// provider, through the same steps as `fiber login`.
    fn store_key(
        &self,
        name: &str,
        label: Option<&str>,
        key: Secret,
    ) -> Result<Stored, ConfigureError>;

    // The `tui.theme` row.

    /// The theme files `tui.theme` can name for `workspace`'s project, by
    /// name, sorted: Fiber home's `themes/` and each enabled installed
    /// extension's. A workspace whose configuration fails to load lists
    /// Fiber home's themes only.
    fn themes(&self, workspace: &Path) -> Vec<String>;

    /// The theme `name` gives `tui.theme` for `workspace`'s project, built
    /// as at start. A workspace whose configuration fails to load reads
    /// Fiber home only.
    fn theme(&self, workspace: &Path, name: &str) -> ThemeSetting;

    // `/tools`.

    /// Every MCP server and extension whose configuration holds tool lists, with them.
    fn tool_switches(&self, workspace: &Path) -> Result<Vec<ToolSwitches>, ConfigureError>;

    /// Switches `tool` (its own name) of `group` on or off in `scope`'s file.
    fn switch_tool(
        &self,
        workspace: &Path,
        group: &ToolGroup,
        tool: &str,
        scope: SwitchScope,
        on: bool,
    ) -> Result<(), ConfigureError>;

    // `/skills`.

    /// Each person file's own `skills.disabled`, for `workspace`'s
    /// project: the repository layer is never read.
    fn skills_disabled(&self, workspace: &Path) -> Result<SkillsDisabled, ConfigureError>;

    /// Switches `name` off or on in `scope`'s own `skills.disabled`:
    /// `on` false adds the name, `on` true removes it.
    fn switch_skill(
        &self,
        workspace: &Path,
        name: &str,
        scope: SwitchScope,
        on: bool,
    ) -> Result<(), ConfigureError>;

    /// The text of the `SKILL.md` at `path`: at most 64 KiB, with `…`
    /// when cut.
    fn skill_text(&self, path: &Path) -> Result<String, ConfigureError>;
}

#[cfg(test)]
mod tests {
    use contract::ErrorCode;

    use super::ConfigureError;

    #[test]
    fn an_error_displays_the_message_the_view_shows() {
        let error = ConfigureError {
            code: ErrorCode::Usage,
            message: "not a number".to_owned(),
        };
        assert_eq!(error.to_string(), "not a number");
    }
}
