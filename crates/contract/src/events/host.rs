//! Payloads of `docs/events.md`, "MCP servers", "Extensions", "Jobs" and
//! "Command acknowledgements".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::action::Progress;
use super::context::{Outcome, SkillSource};
use super::session::Variables;
use crate::shapes::{Failure, Point, Process, Question, Usage, Worktree};
use crate::{CommandId, Envelope, ErrorCode, JobId, SessionId};

/// Why an MCP server failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerFailure {
    /// It failed to start.
    StartFailed,
    /// It missed its startup deadline.
    Deadline,
    /// It needs a login.
    NotLoggedIn,
    /// It died.
    Died,
}

/// `mcp_server_failed` (`docs/mcp.md`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerFailed {
    /// The server's name.
    pub server: String,
    /// Why it failed.
    pub reason: ServerFailure,
    /// Whether Fiber will restart it.
    pub will_restart: bool,
    /// With code `mcp_server_unavailable`.
    pub error: Failure,
}

/// `mcp_server_ready`: a server that died is running again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerReady {
    /// The server's name.
    pub server: String,
}

/// `reloaded`: the new tool set is declared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reloaded {
    /// What happened to each server.
    pub servers: ReloadedServers,
    /// The extensions reloaded.
    pub extensions: Vec<String>,
    /// Each server that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<Vec<ReloadFailure>>,
}

/// The servers a reload kept, restarted, started and stopped, by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadedServers {
    /// Kept as they were.
    pub kept: Vec<String>,
    /// Restarted.
    pub restarted: Vec<String>,
    /// Started.
    pub started: Vec<String>,
    /// Stopped.
    pub stopped: Vec<String>,
}

/// A server that failed during a reload, as on `mcp_server_failed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReloadFailure {
    /// The server's name.
    pub server: String,
    /// Why it failed.
    pub reason: ServerFailure,
    /// The error.
    pub error: Failure,
}

/// `extensions_loaded`: the full set of extensions the session loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionsLoaded {
    /// One object per loaded extension.
    pub extensions: Vec<LoadedExtension>,
}

/// One extension `extensions_loaded` lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedExtension {
    /// The extension's name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// The built-in tools and commands its manifest lists in `replaces`,
    /// in manifest order, as declared: absent on the wire when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<String>,
}

/// What a fork gets of an extension state key (`docs/events.md`,
/// "Extensions").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFork {
    /// The value as of the point.
    AtPoint,
    /// The value at the end of the parent's log. For a delegate's fork,
    /// the end of the parent's log is the fork's point, `forked_from.seq`,
    /// so the first run and every resume get the same values.
    Latest,
    /// Nothing.
    Fresh,
}

/// `extension_state_set`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionStateSet {
    /// The extension's name.
    pub extension: String,
    /// The state key.
    pub key: String,
    /// The key's whole new value, at most 64 KiB.
    pub value: Value,
    /// What a fork gets.
    pub on_fork: OnFork,
}

/// `extension_state_unset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionStateUnset {
    /// The extension's name.
    pub extension: String,
    /// The state key removed.
    pub key: String,
}

/// `extension_ui`: a status line or a widget, never both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionUi {
    /// The extension's name.
    pub extension: String,
    /// The status line or the widget.
    #[serde(flatten)]
    pub ui: Ui,
}

/// What an `extension_ui` line shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Ui {
    /// The extension's status line.
    Status {
        /// The line; `""` clears it.
        status: String,
    },
    /// One of its widgets.
    Widget {
        /// The widget's id.
        widget: String,
        /// Its lines; empty removes it.
        lines: Vec<String>,
    },
}

/// `extension_message`: what an extension's session half sent its own TUI
/// extension with `host.emit`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionMessage {
    /// The extension's name.
    pub extension: String,
    /// What it sent.
    pub data: Value,
}

/// `extension_log`: a line an extension wrote with `host.log`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionLog {
    /// The extension's name.
    pub extension: String,
    /// The line, as the extension wrote it.
    pub message: String,
}

/// `extension_exec`: a program an extension ran outside a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionExec {
    /// The extension's name.
    pub extension: String,
    /// The program.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Its working directory.
    pub cwd: String,
    /// How it ended.
    pub process: Process,
}

/// `job_started`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStarted {
    /// The job's id.
    pub job_id: JobId,
    /// The name of the tool that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The extension that started it with `host.delegate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    /// A short description.
    pub description: String,
    /// The job's output file.
    pub output_path: String,
}

/// `delegate_started` (`docs/delegates.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegateStarted {
    /// The delegate's job.
    pub job_id: JobId,
    /// The delegate's session; on another harness, that harness's own id.
    pub delegate_session_id: SessionId,
    /// The harness, such as `fiber`.
    pub harness: String,
    /// The model reference, with any role resolved.
    pub model: String,
    /// The delegate's workspace.
    pub workspace: String,
    /// When isolated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    /// For a fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<Point>,
}

/// `job_delta`: progress for clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobDelta {
    /// The job.
    pub job_id: JobId,
    /// Its progress, as on `tool_call_delta`.
    #[serde(flatten)]
    pub progress: Progress,
}

/// `job_line`: what a monitor delivered to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobLine {
    /// The monitor's job.
    pub job_id: JobId,
    /// The batch of lines delivered.
    pub lines: String,
    /// Deliveries suppressed since the last one, when any were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed: Option<u64>,
    /// The extensions whose hooks changed the delivered lines, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<Vec<String>>,
}

/// An isolated delegate's worktree when it finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishedWorktree {
    /// Its path and branch.
    #[serde(flatten)]
    pub worktree: Worktree,
    /// Whether it has uncommitted changes.
    pub dirty: bool,
}

/// `delegate_finished`: written just before `job_completed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegateFinished {
    /// The delegate's job.
    pub job_id: JobId,
    /// The final message, bounded.
    pub text: String,
    /// The full final message's path, when cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// When the delegate's turn ended on `ask_user`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<Question>>,
    /// The run's totals.
    pub usage: Usage,
    /// The extensions whose hooks changed the final message, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<Vec<String>>,
    /// When isolated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<FinishedWorktree>,
}

/// `job_completed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobCompleted {
    /// The job.
    pub job_id: JobId,
    /// How it ended.
    pub status: Outcome,
    /// On `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
    /// For a job that ran a process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Process>,
    /// For a failed job, the tail of its output, capped; absent when a hook
    /// changed the completion notice (`notice` holds what the model was sent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tail: Option<String>,
    /// The text the model was sent, when a hook changed or withheld the
    /// completion notice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// The extensions whose hooks changed the completion notice, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<Vec<String>>,
}

/// `jobs_pending_notified`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsPendingNotified {
    /// The jobs named in the notice.
    pub job_ids: Vec<JobId>,
    /// Which notice named them.
    pub reason: PendingReason,
}

/// Why Fiber woke the model about running jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingReason {
    /// The session is about to end.
    Ending,
    /// The session was left unattended: the jobs check.
    Unattended,
}

/// `command_accepted`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandAccepted {
    /// The command's id.
    pub command_id: CommandId,
    /// The command's answer, on the commands in the `command_accepted`
    /// table in `docs/events.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CommandResult>,
}

/// What an accepted command answers with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandResult {
    /// For `rewind`.
    Rewind {
        /// The session that continues this one.
        new_session_id: SessionId,
    },
    /// For `tools`.
    Tools {
        /// One per declared tool.
        tools: Vec<ToolInfo>,
    },
    /// For `mcp_servers`.
    McpServers {
        /// One per configured server.
        servers: Vec<McpServerRow>,
    },
    /// For `commands`.
    Commands {
        /// One per `/name` the session runs.
        commands: Vec<CommandInfo>,
    },
    /// For `skills`.
    Skills {
        /// One per skill discovery found.
        skills: Vec<SkillInfo>,
    },
    /// For `history`: the durable lines in the requested range.
    History {
        /// At most 256 durable event lines.
        lines: Vec<Envelope>,
    },
    /// For `artifact_page`: one page of an artifact's full output.
    ArtifactPage {
        /// The page's text, whole lines from the requested offset.
        text: String,
        /// The byte offset in the file where each line of `text` starts.
        lines: Vec<u64>,
        /// The byte offset after the page; absent at the end of the file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<u64>,
        /// The file's size in bytes.
        size: u64,
    },
    /// For `shell` sent with `send` false, as on `shell_command`.
    Shell {
        /// Its output.
        output: String,
        /// The full output's path, when cut.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<String>,
        /// How it ended.
        process: Process,
    },
    /// For `start`, over the hub: the session the hub started.
    Start {
        /// The session that continues this one.
        session_id: SessionId,
    },
    /// For `status`, over the hub.
    Status {
        /// Whether the hub is running.
        running: bool,
        /// The hub's version.
        fiber_version: String,
        /// The open client connections, this connection's asker included.
        clients: usize,
        /// The environment sessions the hub starts now get, as `session_started` carries it.
        variables: Variables,
    },
    /// For `stop` with `sessions`, over the hub: the sessions that
    /// confirmed closed before the hub exited.
    Stop {
        /// The sessions that confirmed closed.
        sessions_stopped: usize,
    },
    /// For `placeholder_env`, over the hub: the hub's captured values for
    /// the requested placeholder variable names. Absent names are omitted,
    /// never `null`, and the values are never logged.
    PlaceholderEnv {
        /// The present values, by requested name.
        values: BTreeMap<String, String>,
    },
    /// For a nonempty `full` subscribe: the opening fold's boundary.
    Subscribe {
        /// The last durable line sent before the stream goes live.
        last_seq: crate::Seq,
    },
    /// For `quota`.
    Quota {
        /// Cached quota, ordered by provider and credential label.
        quota: Vec<QuotaInfo>,
        /// Percent used at which little is left.
        notice_at: f64,
    },
    /// For `import`: what landed and what was left behind, names only.
    Import {
        /// What the import copied, by final name in Fiber home.
        imported: ImportedNames,
        /// What the import did not copy, with reasons.
        left_behind: Vec<LeftBehindEntry>,
    },
    /// For `offered_file`: the file's text.
    OfferedFile {
        /// The file's full UTF-8 text.
        text: String,
    },
    /// For `credentials`.
    Credentials(CredentialLabels),
    /// For `credential`: the provider the switch applies to.
    CredentialSwitch {
        /// The provider whose credential label switches.
        provider: String,
    },
    /// For `propose_notes`: evidence and proposed lines for one topic.
    ProposeNotes {
        /// Gathered evidence followed by the person's nonempty answer.
        evidence: Vec<crate::notes::Evidence>,
        /// Validated lines citing evidence of a single kind.
        proposed: Vec<crate::notes::Proposed>,
        /// A call or budget failure, with no proposals.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<Box<crate::shapes::Failure>>,
    },
    /// For `call`: what the Lua call returned.
    Call(CallResult),
}

/// The `call` command's answer: the return wrapped as `value`, absent when
/// the call returns nothing and distinct from a present JSON null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallResult {
    /// The return; any JSON value. Absent when the call returns nothing.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub value: Option<Value>,
}

/// Reads a present JSON value as `Some`, keeping an explicit `null` apart
/// from an absent key: `{"value": null}` is a null return, while `{}` is no
/// return. A plain `Option` reads both as `None`.
fn present_value<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

/// The labels of the provider a credential command would switch now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialLabels {
    /// The provider's name.
    pub provider: String,
    /// Its credential labels, sorted.
    pub credentials: Vec<String>,
    /// The label the provider uses now; absent until used, or when scripted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// One provider credential label's cached quota, with no age.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaInfo {
    /// The provider's name.
    pub provider: String,
    /// The credential label.
    pub credential: String,
    /// Reported windows, in vendor order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<QuotaWindow>>,
    /// Reported credit, instead of windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<QuotaCredit>,
    /// The last fetch or credential read failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreachable: Option<crate::shapes::True>,
}

/// A quota window reports either its usage or that it has reset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    /// The window's name.
    pub name: String,
    /// Percent used, absent after reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_used: Option<f64>,
    /// The window's reset time has passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<crate::shapes::True>,
    /// When the window resets, in milliseconds since the epoch, where the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
}

/// A provider's remaining credit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaCredit {
    /// Remaining credit in the provider's units.
    pub remaining: f64,
    /// The limit, when the provider reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

/// What a one-time import copied, by final name in Fiber home: names only,
/// never secret values (`docs/configuration.md`, "Importing").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedNames {
    /// MCP servers written under `mcp.servers`, by name.
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    /// Skills written under `skills/`, by directory name.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Flat commands and prompts wrapped as prompt templates, by skill
    /// directory name.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Instruction files written at the top of Fiber home, by file name.
    #[serde(default)]
    pub instructions: Vec<String>,
}

/// One source item the import did not copy, and why: names only, never a
/// secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeftBehindEntry {
    /// The source item, by path or name, never a value.
    pub item: String,
    /// Why it was not imported.
    pub reason: String,
}

/// One declared tool (`docs/tools.md`, "Seeing the tools").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    /// The tool's name.
    pub name: String,
    /// Where it comes from.
    #[serde(flatten)]
    pub source: ToolSource,
    /// Whether it is sent in full, deferred or loaded.
    pub state: ToolState,
    /// Its size in bytes.
    pub bytes: u64,
    /// Its estimated tokens; absent before the first request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

/// One configured MCP server, as `mcp_servers` lists it
/// (`docs/mcp.md`, "Servers, their state and the per-server commands").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerRow {
    /// The server's name.
    pub name: String,
    /// The layer that declares it.
    pub layer: McpServerLayer,
    /// Its state.
    pub state: McpServerState,
    /// The failure's sentence; empty when the server did not fail.
    pub reason: String,
    /// Its tool count; absent while `connecting`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<u64>,
    /// False only on a repository's server nobody approved.
    pub approved: bool,
    /// The login URL; only while a login waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
}

/// The configuration layer that declares an MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerLayer {
    /// The global configuration.
    Global,
    /// The per-project configuration.
    Project,
    /// The repository's configuration.
    Repository,
}

/// An MCP server's state, as `mcp_servers` reports it (`docs/mcp.md`,
/// "Servers, their state and the per-server commands").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpServerState {
    /// Declared but never started this session, with no failure recorded.
    #[serde(rename = "connecting")]
    Connecting,
    /// Running, with its tools declared.
    #[serde(rename = "ready")]
    Ready,
    /// The last start failed; the row carries its sentence.
    #[serde(rename = "failed")]
    Failed,
    /// The last start failed `NotLoggedIn` with no later ready.
    #[serde(rename = "needs-login")]
    NeedsLogin,
}

/// One skill discovery found (`docs/invocation.md`, "What each command
/// does", `skills`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInfo {
    /// The skill's name.
    pub name: String,
    /// A one-line description.
    pub description: String,
    /// Its `SKILL.md`, as discovery logs it.
    pub path: String,
    /// Where it was found.
    pub source: SkillSource,
    /// The extension's name, exactly when the source is an extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    /// Whether its header and place let the model load it.
    pub model_invocable: bool,
    /// Whether `skills.disabled` switches it off.
    pub disabled: bool,
    /// The `SKILL.md` paths of the skills it shadows.
    pub shadows: Vec<String>,
    /// The winning skill's `SKILL.md` path, exactly on a shadowed skill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<String>,
}

/// One `/name` the session runs (`docs/invocation.md`, "What each command
/// does", `commands`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    /// The name typed after `/`.
    pub name: String,
    /// A one-line description.
    pub description: String,
    /// The skill's `argument-hint`, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    /// `skill`, `template`, the extension's name, or the MCP server's name.
    pub tag: String,
    /// True on an extension's command, absent otherwise.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub extension: bool,
}

/// Where a tool comes from, keyed by `source`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ToolSource {
    /// Built in.
    Builtin,
    /// An extension.
    Extension {
        /// The extension.
        extension: String,
    },
    /// An MCP server.
    Mcp {
        /// The server.
        server: String,
        /// The server's own name for the tool, which its `tools.enabled`
        /// and `tools.disabled` name (`docs/mcp.md`, "Tools and their names").
        tool: String,
    },
}

/// How a tool is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    /// Sent in full.
    Full,
    /// Deferred.
    Deferred,
    /// Deferred and since loaded.
    Loaded,
}

/// `command_rejected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRejected {
    /// The command's id; absent when the line was `malformed` and carried
    /// none that could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<CommandId>,
    /// The rejection code (`docs/invocation.md`, "Driver commands").
    pub code: ErrorCode,
    /// Fiber's own sentence.
    pub message: String,
}
