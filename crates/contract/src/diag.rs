//! The debug level's seam (`docs/state.md`, "Diagnostic logs"): what one
//! `provider_request` line records, the operations an `op_timing` line may
//! name, and the sinks a caller hands them to. Like
//! [`crate::clock::Clock`], it defines the seam and contains no behaviour;
//! `log::diag::Diag` writes the lines.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use crate::provider::{ModelCall, ModelRequest};

/// Redacted digests of one request's cacheable prefix segments, in the
/// protocol's own encoding (`docs/prompt-cache.md`, "Cache diagnostics").
/// The digests stand in for the bytes: no prompt, tool or model text
/// reaches the line. `system` and `tools` are absent when the request
/// sends no such segment; `messages[i]` covers the cumulative
/// conversation encoding through message boundary `i`. Digests compare
/// within one protocol only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrefixHashes {
    /// The system prompt segment's digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The tool definitions segment's digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<String>,
    /// Each cumulative message-prefix segment's digest, in order.
    pub messages: Vec<String>,
}

/// Why Fiber made a provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// A model call: a turn's, the reviewer's or a cache-warming refresh.
    ModelCall,
    /// A provider's model-list refresh.
    ModelList,
    /// A quota read.
    Quota,
    /// An OAuth token refresh.
    TokenRefresh,
    /// A provider's `cost()` lookup.
    Cost,
}

/// The `data` of one `provider_request` line. The fields serialize in the
/// order declared here, and an absent option is left out, never `null`.
/// Nothing here may hold a credential, header value, query string, prompt or
/// model text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderRequest {
    /// The provider's name.
    pub provider: String,
    /// The model, on a model call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Why the request was made.
    pub purpose: Purpose,
    /// The URL's authority, without userinfo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The URL's path, with no query string or fragment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The HTTP status, when a response came.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Which attempt this was, from 1.
    pub attempt: u32,
    /// The request body's length.
    pub request_bytes: u64,
    /// The response body bytes read; 0 when no response came.
    pub response_bytes: u64,
    /// Milliseconds from the send to the response headers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers_ms: Option<u64>,
    /// Milliseconds from the send to the first streamed token, on a model call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    /// Milliseconds from the send until the call returned.
    pub total_ms: u64,
    /// Redacted digests of the request's cacheable prefix segments, when
    /// the `cache.diagnostics` setting asked for them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_hashes: Option<PrefixHashes>,
    /// Input tokens the response read from the cache, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    /// Input tokens the response wrote to the cache, keyed by cache-write
    /// billing class (`"5m"`, `"1h"`), not the vendor lifetime (OpenAI's
    /// 30-minute cache reports under `"5m"`), when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<BTreeMap<String, u64>>,
}

/// Where debug lines go. `provider_request` writes nothing when
/// [`DebugLog::debugging`] is false, so a caller may skip building the value.
pub trait DebugLog: Send + Sync {
    /// Whether debug lines are written.
    fn debugging(&self) -> bool;

    /// Writes one `provider_request` line, when debugging.
    fn provider_request(&self, request: &ProviderRequest);
}

/// Builds one observed model call for `request`: the loop calls the
/// session's factory for a turn's call with its attempt, the reviewer's
/// factory for a review with the first attempt, and the session's factory
/// for a warming refresh with the first attempt. The ordinal starts at 1,
/// so 0 is unrepresentable. A factory captures its route's identity and
/// never infers it from mutable session state.
pub type Observe = Arc<dyn Fn(&ModelRequest, NonZeroU32) -> Box<dyn ModelCall> + Send + Sync>;

/// One operation an `op_timing` line may name (`docs/state.md`,
/// "Diagnostic logs"): the `op` in its `data`. The list is closed: a new
/// module or top-level operation adds its variant in the pull request that
/// needs it, a one-line change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Reading the configuration.
    StartupConfig,
    /// Loading extensions.
    StartupExtensions,
    /// Building the preamble: the system prompt, tool definitions and
    /// request settings.
    StartupPreamble,
    /// Bringing the session socket up until it accepts.
    StartupSocket,
    /// Picking the model, reading its credential, starting its Lua
    /// provider and finding the reviewer.
    StartupRouting,
    /// A model switch through routing.
    RoutingSwitch,
    /// Reading the session log for the terminal's first attach.
    TuiLogRead,
    /// Building the transcript for the terminal's first attach.
    TuiTranscript,
    /// The terminal's first frame.
    TuiFirstRender,
    /// The first draw holding attached-session history.
    TuiAttachedFrame,
    /// A terminal frame past its 16 ms budget; one within budget writes
    /// no line.
    TuiFrame,
    /// Loading TUI extensions before the terminal's first frame.
    TuiExtensions,
    /// The opening summary subscribe through its acceptance, which queues
    /// the session's commands line.
    TuiAttachSummary,
    /// The open's pre-frame tail history read, to completion.
    TuiTailWait,
    /// Folding one tail answer into the preview candidate.
    TuiPreviewFold,
    /// Installing the open's pre-frame preview candidate, without
    /// another fold.
    TuiTailInstall,
    /// One session-log write attempt.
    LogAppend,
    /// One session-log sync, when the durability policy syncs.
    LogFsync,
    /// A tool call's permission order, the reviewer included.
    ToolPermission,
    /// The reviewer's stages alone, when it runs.
    ToolReviewer,
    /// An actual before or after hook chain invocation on a tool call.
    ToolHooks,
    /// A tool call's execution, not its permission, review or hooks.
    ToolRun,
    /// Assembling a model request from the session's history.
    TurnRequest,
    /// A model call, from sending the request until the stream ends.
    TurnModelCall,
    /// One handoff, end to end.
    TurnHandoff,
    /// One hook chain other than the tool hooks, named by the chain.
    Hook(HookChain),
    /// One MCP server, from starting or dialing it until its tool list
    /// is in hand.
    McpConnect,
    /// A background job or delegate, until it runs.
    JobsStart,
    /// Creating a git worktree a session or delegate runs in.
    WorktreeCreate,
    /// Removing one.
    WorktreeRemove,
    /// One image child's work, from spawning it to its answer.
    PictureRun,
    /// A hub session start, until the session's socket accepts.
    HubSessionStart,
    /// Resuming a session through the hub.
    HubResume,
    /// Replaying a subscription to its acknowledgement.
    HubSubscribeReplay,
    /// Listing a project's sessions.
    SessionsList,
    /// Opening a connection.
    NetConnect,
    /// One full session-search scan request.
    SessionSearchScan,
}

impl Op {
    /// The operation's name on the wire: a hook chain reads
    /// `hook.<chain>`, every other operation its dotted path. A pure
    /// conversion of the value, as the seam holds no behaviour.
    pub const fn name(self) -> &'static str {
        match self {
            Self::StartupConfig => "startup.config",
            Self::StartupExtensions => "startup.extensions",
            Self::StartupPreamble => "startup.preamble",
            Self::StartupSocket => "startup.socket",
            Self::StartupRouting => "startup.routing",
            Self::RoutingSwitch => "routing.switch",
            Self::TuiLogRead => "tui.log_read",
            Self::TuiTranscript => "tui.transcript",
            Self::TuiFirstRender => "tui.first_render",
            Self::TuiAttachedFrame => "tui.attached_frame",
            Self::TuiFrame => "tui.frame",
            Self::TuiExtensions => "tui.extensions",
            Self::TuiAttachSummary => "tui.attach_summary",
            Self::TuiTailWait => "tui.tail_wait",
            Self::TuiPreviewFold => "tui.preview_fold",
            Self::TuiTailInstall => "tui.tail_install",
            Self::LogAppend => "log.append",
            Self::LogFsync => "log.fsync",
            Self::ToolPermission => "tool.permission",
            Self::ToolReviewer => "tool.reviewer",
            Self::ToolHooks => "tool.hooks",
            Self::ToolRun => "tool.run",
            Self::TurnRequest => "turn.request",
            Self::TurnModelCall => "turn.model_call",
            Self::TurnHandoff => "turn.handoff",
            Self::Hook(chain) => chain.hook_name(),
            Self::McpConnect => "mcp.connect",
            Self::JobsStart => "jobs.start",
            Self::WorktreeCreate => "worktree.create",
            Self::WorktreeRemove => "worktree.remove",
            Self::PictureRun => "picture.run",
            Self::HubSessionStart => "hub.session_start",
            Self::HubResume => "hub.resume",
            Self::HubSubscribeReplay => "hub.subscribe_replay",
            Self::SessionsList => "sessions.list",
            Self::NetConnect => "net.connect",
            Self::SessionSearchScan => "session_search.scan",
        }
    }
}

impl Serialize for Op {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

/// One hook chain an `op_timing` line may time: every chain other than the
/// tool hooks, which `tool.hooks` covers. The operation's suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookChain {
    /// A session's start.
    SessionStart,
    /// A turn's start.
    TurnStart,
    /// Before one message.
    BeforeMessage,
    /// Before one model call.
    BeforeModelCall,
    /// A turn's end.
    TurnEnd,
    /// Before a handoff's note is written.
    BeforeHandoff,
}

impl HookChain {
    /// The `hook.<chain>` operation's name, and its wire form: `Op`'s
    /// serialization writes this, which the operation test pins.
    pub const fn hook_name(self) -> &'static str {
        match self {
            Self::SessionStart => "hook.session_start",
            Self::TurnStart => "hook.turn_start",
            Self::BeforeMessage => "hook.before_message",
            Self::BeforeModelCall => "hook.before_model_call",
            Self::TurnEnd => "hook.turn_end",
            Self::BeforeHandoff => "hook.before_handoff",
        }
    }
}

/// Starts an operation's timer: the seam a restricted crate takes instead
/// of depending on `log`. It invokes the same `log::diag::timed` every
/// other timed place uses, so there is one timing implementation. An
/// explicit stamp suppresses the start read, never the end read. `None`
/// at `info`, before any clock read. The guard is owned and `Send`, so it
/// may follow the work to its worker; dropping it without stopping stamps
/// the drop time.
pub type Timing =
    Arc<dyn Fn(Op, Option<&str>, Option<Instant>) -> Option<Box<dyn Send>> + Send + Sync>;

#[cfg(test)]
#[path = "diag_tests.rs"]
mod tests;
