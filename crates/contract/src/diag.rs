//! The debug level's seam (`docs/state.md`, "Diagnostic logs"): what one
//! `http_request` line records, the operations an `op_timing` line may
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

/// Why Fiber made an HTTP request. A provider function keeps the
/// provider purpose it already had; every Lua or process `host.http`
/// call outside a provider function records `Extension`, each MCP-client
/// request records `Mcp`, and release tag, checksum and binary downloads
/// record `ExtensionRelease`.
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
    /// One `web_fetch` request: each actual HTTP request, including each
    /// redirect hop, writes its own line.
    WebFetch,
    /// One MCP-client request: each POST, and each GET or DELETE the
    /// streamable HTTP transport sends, plus login, discovery and token
    /// requests.
    Mcp,
    /// One Lua or process `host.http` call outside a provider function:
    /// a hook, command, timer, tool or the entry script.
    Extension,
    /// One extension release tag lookup, checksum or binary download.
    ExtensionRelease,
}

/// The `data` of one `http_request` line. The fields serialize in the
/// order declared here, and an absent option is left out, never `null`.
/// Nothing here may hold a credential, header value, query string, prompt or
/// model text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HttpRequest {
    /// The provider's name, on a provider's purposes only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The extension's registered name, on `extension` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
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

/// Builds one `HttpRequest` skeleton per purpose class: identity and
/// purpose are fixed, every other field starts absent or zero and the
/// caller stamps the request's own values. Provider purposes keep the
/// provider name and never an extension; `web_fetch` and `mcp` carry no
/// identity, model, host or path; `extension` carries the extension's
/// registered name; `extension_release` carries the sanitized host and
/// path of a release URL, which holds a tag and an asset name and no
/// secret.
impl HttpRequest {
    /// One request on a provider's purposes: `model_call`, `model_list`,
    /// `quota`, `token_refresh` or `cost`.
    pub fn for_provider(provider: impl Into<String>, purpose: Purpose) -> Self {
        Self {
            provider: Some(provider.into()),
            extension: None,
            model: None,
            purpose,
            host: None,
            path: None,
            status: None,
            attempt: 1,
            request_bytes: 0,
            response_bytes: 0,
            headers_ms: None,
            first_token_ms: None,
            total_ms: 0,
            prefix_hashes: None,
            cache_read: None,
            cache_write: None,
        }
    }

    /// One `web_fetch` request.
    pub fn for_web_fetch() -> Self {
        Self {
            provider: None,
            extension: None,
            model: None,
            purpose: Purpose::WebFetch,
            host: None,
            path: None,
            status: None,
            attempt: 1,
            request_bytes: 0,
            response_bytes: 0,
            headers_ms: None,
            first_token_ms: None,
            total_ms: 0,
            prefix_hashes: None,
            cache_read: None,
            cache_write: None,
        }
    }

    /// One MCP-client request.
    pub fn for_mcp() -> Self {
        Self {
            purpose: Purpose::Mcp,
            ..Self::for_web_fetch()
        }
    }

    /// One `host.http` call outside a provider function.
    pub fn for_extension(extension: impl Into<String>) -> Self {
        Self {
            extension: Some(extension.into()),
            purpose: Purpose::Extension,
            ..Self::for_web_fetch()
        }
    }

    /// One extension release tag lookup, checksum or binary download.
    pub fn for_extension_release(host: Option<String>, path: Option<String>) -> Self {
        Self {
            purpose: Purpose::ExtensionRelease,
            host,
            path,
            ..Self::for_web_fetch()
        }
    }
}

/// Where debug lines go. `http_request` writes nothing when
/// [`DebugLog::debugging`] is false, so a caller may skip building the value.
pub trait DebugLog: Send + Sync {
    /// Whether debug lines are written.
    fn debugging(&self) -> bool;

    /// Writes one `http_request` line, when debugging.
    fn http_request(&self, request: &HttpRequest);
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
    /// A terminal frame past its 16 ms budget; one within budget writes
    /// no line.
    TuiFrame,
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
            Self::TuiFrame => "tui.frame",
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
