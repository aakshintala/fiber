//! The seam the configuration views read and write through
//! (`docs/tui.md`, "Swapped views"). The terminal never reads
//! configuration itself (`docs/architecture.md`, "The call rules"): `main`
//! implements [`Configure`] and passes it in [`crate::Launch`]. Every call
//! is a few small file reads or one locked write, made only on a person's
//! action, so it runs on the loop's thread.

use std::fmt;
use std::path::{Path, PathBuf};

use contract::ErrorCode;

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

    // The `tui.theme` row.

    /// The theme files in Fiber home's `themes/`, by name, sorted.
    fn themes(&self) -> Vec<String>;

    /// The theme `name` gives `tui.theme`, built as at start.
    fn theme(&self, name: &str) -> ThemeSetting;

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
