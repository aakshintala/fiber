//! The seam the configuration views read and write through
//! (`docs/tui.md`, "Swapped views"). The terminal never reads
//! configuration itself (`docs/architecture.md`, "The call rules"): `main`
//! implements [`Configure`] and passes it in [`crate::Launch`]. Every call
//! is a few small file reads or one locked write, made only on a person's
//! action, so it runs on the loop's thread, except [`BrowserLogin::run`],
//! which blocks on a worker thread until the login ends.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contract::ErrorCode;
use contract::Secret;
use contract::clock::Clock;

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

/// One `/logout` row: a provider's label or a declared secret, by name.
/// A provider lists one row per label it has, stored or configured; a
/// secret lists one row. Separate variants keep contradictory states
/// unbuildable: a secret has no label, no default mark, no origin and
/// no siblings (`docs/code-quality.md`, "Types").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogoutRow {
    /// One of a provider's labels, stored or configured.
    Provider {
        /// The provider's name.
        name: String,
        /// The provider's label.
        label: String,
        /// Whether this is the provider's selected label.
        default: bool,
        /// Whether Enter may delete it: false for a key from an
        /// environment variable, a file or a command.
        deletable: bool,
        /// Where a non-deletable key comes from, described without its
        /// secret; `None` on a deletable row.
        origin: Option<String>,
        /// The other providers reading the same stored directory.
        siblings: Vec<String>,
    },
    /// One declared secret.
    Secret {
        /// The secret's name.
        name: String,
        /// Whether Enter may delete it: false with nothing stored.
        deletable: bool,
    },
}

impl LogoutRow {
    /// The provider or secret's name.
    pub fn name(&self) -> &str {
        match self {
            LogoutRow::Provider { name, .. } | LogoutRow::Secret { name, .. } => name,
        }
    }

    /// The provider's label; a secret has none.
    pub fn label(&self) -> Option<&str> {
        match self {
            LogoutRow::Provider { label, .. } => Some(label),
            LogoutRow::Secret { .. } => None,
        }
    }

    /// Whether Enter may delete it.
    pub fn deletable(&self) -> bool {
        match self {
            LogoutRow::Provider { deletable, .. } | LogoutRow::Secret { deletable, .. } => {
                *deletable
            }
        }
    }

    /// The other providers reading the same stored directory; empty on
    /// a secret.
    pub fn siblings(&self) -> &[String] {
        match self {
            LogoutRow::Provider { siblings, .. } => siblings,
            LogoutRow::Secret { .. } => &[],
        }
    }
}

/// What a login stored: the file under Fiber home, and whether it replaced one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The file under Fiber home, such as `credentials/acme/default`.
    pub path: String,
    /// Whether it replaced a stored secret.
    pub replaced: bool,
}

/// One presence-gap placeholder of a provider: its setting with its
/// declared environment variable, which is `None` when the placeholder
/// declares none (`docs/model-routing.md`, "A per-account host").
/// Never a scope: the answer always saves globally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostGap {
    /// The placeholder's setting name.
    pub name: String,
    /// The environment variable read when the setting is unset.
    pub env: Option<String>,
}

/// What saving one host answer reported: the project settings file
/// hiding the saved value, when a project-layer empty masks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSaved {
    /// The project settings file whose empty value hides the saved
    /// global one, if one masks it.
    pub masked: Option<PathBuf>,
}

/// The reviewer choice for one provider: its models as local IDs in its
/// order, with the saved pick or package default preselected. Empty
/// `models` is an explicit skip, even when `selected` names a saved pick.
/// Holds only local IDs and public notices, never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerModels {
    /// The provider's model IDs, in its order.
    pub models: Vec<String>,
    /// The saved pick, else the package default, kept even when the list
    /// does not name it.
    pub selected: Option<String>,
    /// One line per discovery problem, already redacted.
    pub notices: Vec<String>,
}

/// The installed extensions and the outcome text for the model picker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderInstall {
    /// Each extension put in place.
    pub installed: Vec<String>,
    /// The install summary, one line per element.
    pub summary: Vec<String>,
    /// Providers whose selected credential is missing or unreadable.
    pub login: Vec<LoginNeed>,
}

/// A provider needing a key, or whose key could not be checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginNeed {
    /// The provider to pass to `fiber login`.
    pub provider: String,
    /// A credential-read failure; absent when the key is missing.
    pub error: Option<String>,
}

/// One notes layer's file with its own `reviewer.context` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotesFile {
    /// The layer's file, named whether or not it exists.
    pub file: PathBuf,
    /// The layer's own `reviewer.context`, empty when unset or when the
    /// file is missing; whitespace is kept as is.
    pub text: String,
    /// 16 lowercase hex digits of the file's bytes, or `absent` when the
    /// file does not exist.
    pub version: String,
}

/// Both person layers' notes files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerNotes {
    /// Fiber home's `config.json` notes.
    pub global: NotesFile,
    /// The project's `config.json` notes.
    pub project: NotesFile,
}

/// One sent notes layer: the version its read saw, and the text to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotesWrite {
    /// The version [`Configure::reviewer_notes`] read.
    pub version: String,
    /// The `reviewer.context` to store.
    pub text: String,
}

/// What a notes save did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotesSaved {
    /// A sent version no longer matched: nothing was written.
    Stale,
    /// Every sent layer's outcome, global first.
    Written(Vec<FileSaved>),
}

/// One sent file's outcome: three states, so "not saved, no error"
/// cannot be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSaved {
    /// The layer's file.
    pub file: PathBuf,
    /// Whether it was written, and any error.
    pub outcome: FileOutcome,
}

/// Whether one notes file was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOutcome {
    /// The file holds the new notes.
    Saved,
    /// The rename landed but syncing the directory failed, so the new
    /// notes are in place with an error.
    SavedUnsynced(ConfigureError),
    /// The file is byte for byte as before, with why.
    NotSaved(ConfigureError),
}

/// What a revoke did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revoked {
    /// The line was deleted.
    Removed,
    /// The line had moved, changed or gone: nothing was written.
    Stale,
}

/// One `/keys` change to save: the action's id, and its new entry.
/// `None` removes `keys.<id>`; `Some` sets it to the names in written
/// form, `Some(vec![])` unbinding the action
/// (`docs/configuration.md`, "Keys").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEdit {
    /// The action's id.
    pub id: String,
    /// The new entry, or `None` to remove it.
    pub keys: Option<Vec<String>>,
}

/// Shows a browser login's URLs: `open` shows `url` and asks the terminal
/// to open it; `show` shows the `code` to enter at `url` and opens nothing
/// (`docs/tui.md`, "Logging in").
pub trait LoginShow: Send + Sync {
    /// Shows `url` and asks the terminal to open it.
    fn open(&self, url: &str);
    /// Shows the `code` to enter at `url`; opens nothing.
    fn show(&self, url: &str, code: &str);
}

/// A browser login `Configure::browser_login` started: `run` blocks until
/// the login ends and is called once, off the loop thread; `cancel`
/// returns once nothing more can be stored, or once a store under way has
/// finished.
pub trait BrowserLogin: Send + Sync {
    /// Blocks until the login ends: what `fiber login` stores.
    fn run(&self) -> Result<Stored, ConfigureError>;
    /// Cancels the waiting login, storing nothing further.
    fn cancel(&self);
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
    // The model picker.

    /// Installs the first-party package serving `provider`, without asking.
    /// Runs only off the loop thread: the install fetches and blocks.
    fn install_provider(
        &self,
        workspace: &Path,
        provider: &str,
        clock: Arc<dyn Clock>,
    ) -> Result<ProviderInstall, ConfigureError>;

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

    /// Whether the first-run import offer shows: true when neither
    /// source has a completed receipt, so the offer fires at most
    /// until the first import lands.
    fn import_pending(&self) -> bool;

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

    /// Starts the browser login for `name`, showing its URLs through
    /// `shown`: returns at once and does no I/O. The returned login's
    /// `run` blocks until the login ends and runs only off the loop
    /// thread; `cancel` returns once nothing more can be stored.
    fn browser_login(
        &self,
        name: &str,
        shown: Arc<dyn LoginShow>,
        clock: Arc<dyn Clock>,
    ) -> Arc<dyn BrowserLogin>;

    /// Reads the reviewer choice for the installed provider `name`: its
    /// models with the saved global pick preselected, else the package
    /// default. Runs only off the loop thread: discovery blocks.
    fn reviewer_models(
        &self,
        name: &str,
        clock: Arc<dyn Clock>,
    ) -> Result<ReviewerModels, ConfigureError>;

    /// Saves `id` as provider `name`'s reviewer, exactly as listed: one
    /// main-owned global write. Save errors name the key.
    fn set_reviewer(&self, name: &str, id: &str) -> Result<(), ConfigureError>;

    /// One presence-gap placeholder of provider `name`: its setting with
    /// its declared environment variable, which is `None` when the
    /// placeholder declares none.
    ///
    /// A gap exists exactly when the placeholder has no present value —
    /// no setting string and no environment-variable string — regardless
    /// of validity (`docs/model-routing.md`, "A per-account host").
    /// The view reads this after storing a credential; the worker reads
    /// it after a browser login stores one.
    fn gaps(&self, name: &str) -> Result<Vec<HostGap>, ConfigureError>;

    /// Saves `value` as placeholder `key`'s answer for provider `name`:
    /// one main-owned global write, the same file `fiber config set` and
    /// `host.config.set(extension, "machine", ...)` write
    /// (`docs/configuration.md`, "Extension settings"). A value that is
    /// not a host is refused with a usage error naming the setting, never
    /// the value, and nothing is written. A project-layer empty masking
    /// the saved value comes back as the project settings file hiding it.
    fn save_host(&self, name: &str, key: &str, value: &str) -> Result<HostSaved, ConfigureError>;

    // `/logout`.

    /// Every stored provider label and declared secret, providers in
    /// `login_targets` order then secrets: one row per provider label,
    /// stored or configured, with the selected label marked, then one row
    /// per declared secret. A row from an environment variable, a file or
    /// a command is not deletable and carries its origin.
    fn logout_rows(&self) -> Result<Vec<LogoutRow>, ConfigureError>;

    /// Deletes the stored credential `name` holds under `label` (`None`
    /// for a secret), through the same steps as `fiber logout`: the file
    /// only, leaving `providers."<name>".credential` as is. Answers the
    /// file under Fiber home, such as `credentials/acme/work`.
    fn delete_stored(&self, name: &str, label: Option<String>) -> Result<String, ConfigureError>;

    /// Both person layers' notes for `workspace`'s project, each with its
    /// file, its own `reviewer.context` and its version.
    fn reviewer_notes(&self, workspace: &Path) -> Result<ReviewerNotes, ConfigureError>;

    /// Saves the sent layers' notes, global first then project: `stale`
    /// writes nothing, `written` carries one entry per sent layer. A
    /// layer not sent is not written and has no entry.
    fn save_reviewer_notes(
        &self,
        workspace: &Path,
        global: Option<NotesWrite>,
        project: Option<NotesWrite>,
    ) -> Result<NotesSaved, ConfigureError>;

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

    // `/keys`.

    /// Saves the `/keys` screen's edits to the global `keys`, in one
    /// write. Called only with a non-empty slice.
    fn save_keys(&self, edits: &[KeyEdit]) -> Result<(), ConfigureError>;
}
