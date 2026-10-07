//! Payloads of `docs/events.md`, "MCP servers", "Extensions", "Jobs" and
//! "Command acknowledgements".

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::action::Progress;
use super::context::Outcome;
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
}

/// What a fork gets of an extension state key (`docs/events.md`,
/// "Extensions").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFork {
    /// The value as of the point.
    AtPoint,
    /// The value at the end of the parent's log.
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
    /// For a failed job, the tail of its output, capped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tail: Option<String>,
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
    /// For `commands`.
    Commands {
        /// One per `/name` the session runs.
        commands: Vec<CommandInfo>,
    },
    /// For `history`: the durable lines in the requested range.
    History {
        /// At most 256 durable event lines.
        lines: Vec<Envelope>,
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
    },
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
    /// `skill`, `template`, or the extension's name.
    pub tag: String,
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
