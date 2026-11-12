//! The error codes (`docs/errors.md`), the notice codes and the driver command
//! rejection codes (`docs/invocation.md`, "Driver commands"). They share one
//! namespace: a label means the same thing wherever it appears.

use serde::{Deserialize, Serialize};

/// Declares [`ErrorCode`] and, for tests, the list of every known code.
/// Each variant declares its stable `snake_case` label beside it, so a new
/// code cannot be added without its label.
macro_rules! codes {
    ($($(#[doc = $doc:literal])+ $name:ident = $label:literal,)+) => {
        /// A stable label a consumer switches on, never parsing the message.
        /// Codes are an open set: a code this build does not know reads as
        /// [`ErrorCode::Other`] and writes back unchanged.
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum ErrorCode {
            $($(#[doc = $doc])+ $name,)+
            /// A code this build does not know; a generic failure.
            #[serde(untagged)]
            Other(String),
        }

        impl ErrorCode {
            /// Returns the variant's stable label: the declared `snake_case`
            /// text for a known code, the kept text for [`ErrorCode::Other`].
            pub fn name(&self) -> &str {
                match self {
                    $(Self::$name => $label,)+
                    Self::Other(text) => text,
                }
            }

            /// Reads a label back into its variant: every declared label
            /// returns that variant, and anything else — unknown hub codes,
            /// the empty string, mixed case — is kept as
            /// [`ErrorCode::Other`] with the text unchanged.
            pub fn from_name(name: &str) -> Self {
                match name {
                    $($label => Self::$name,)+
                    name => Self::Other(name.to_owned()),
                }
            }
        }

        #[cfg(test)]
        const KNOWN: &[ErrorCode] = &[$(ErrorCode::$name,)+];
    };
}

codes! {
    /// An edit block's text occurs more than once.
    AmbiguousMatch = "ambiguous_match",
    /// The provider rejected the credential.
    AuthenticationFailed = "authentication_failed",
    /// The block budget ran out with no human to answer.
    Blocked = "blocked",
    /// `web_fetch` named a link-local address or a cloud metadata host.
    BlockedHost = "blocked_host",
    /// The spending budget was reached, or an extension refused a model
    /// request.
    BudgetExceeded = "budget_exceeded",
    /// A turn is running.
    Busy = "busy",
    /// The terminal rebuilt its completion cache after a model switch.
    CacheRebuild = "cache_rebuild",
    /// A `cancel` stopped a `shell` before its command started.
    Cancelled = "cancelled",
    /// The session was sent `close`.
    Closing = "closing",
    /// Two extensions registered the same command name.
    CommandConflict = "command_conflict",
    /// A configuration file is invalid.
    ConfigInvalid = "config_invalid",
    /// An unknown key, or a key a repository may not set.
    ConfigKeyIgnored = "config_key_ignored",
    /// The connection to the provider failed.
    ConnectionFailed = "connection_failed",
    /// The request does not fit the context window.
    ContextOverflow = "context_overflow",
    /// A stored credential cannot be used, or the provider's `credential()` or
    /// `sign()` failed.
    CredentialFailed = "credential_failed",
    /// No credential was found for the session's model.
    CredentialMissing = "credential_missing",
    /// A command named a delegate's session.
    DelegateSession = "delegate_session",
    /// A delegate tool at depth 2.
    DepthExceeded = "depth_exceeded",
    /// A command repeats the id of one the session already accepted.
    DuplicateCommand = "duplicate_command",
    /// An extension failed to start or missed its deadline.
    ExtensionFailed = "extension_failed",
    /// An extension needs a newer `fiber` or a different extension API version.
    ExtensionIncompatible = "extension_incompatible",
    /// The session model's provider is not installed.
    ExtensionMissing = "extension_missing",
    /// An install names a repository or tag that does not exist.
    ExtensionNotFound = "extension_not_found",
    /// `fiber extension remove` names an extension nothing is installed under.
    ExtensionNotInstalled = "extension_not_installed",
    /// A required extension failed to start.
    ExtensionRequiredFailed = "extension_required_failed",
    /// A repository's approved copy of an extension replaced the personal
    /// install of the same name in that project.
    ExtensionShadowed = "extension_shadowed",
    /// A repository's required extension is not approved.
    ExtensionUnapproved = "extension_unapproved",
    /// The extension providing the tool died twice.
    ExtensionUnavailable = "extension_unavailable",
    /// An install or update could not fetch: git or the network failed.
    FetchFailed = "fetch_failed",
    /// A monitor was suppressed for 30 seconds.
    Flooded = "flooded",
    /// A connection from inside a session is refused this command.
    FromSession = "from_session",
    /// The terminal is still reading the session's history.
    HistoryLoading = "history_loading",
    /// An extension hook errored or ran out of time.
    HookFailed = "hook_failed",
    /// A repository's required hook is not approved.
    HookUnapproved = "hook_unapproved",
    /// `web_fetch` got a status other than 2xx.
    HttpError = "http_error",
    /// The hub runs another Fiber version or commit than this terminal.
    HubVersion = "hub_version",
    /// Fiber cannot tell whether the call completed.
    Indeterminate = "indeterminate",
    /// The instruction text passes 10% of the context window.
    InstructionsLarge = "instructions_large",
    /// Arguments failed their schema or checks.
    InvalidArguments = "invalid_arguments",
    /// The provider rejected the request for any other reason.
    InvalidRequest = "invalid_request",
    /// A filesystem failure; the message names the path.
    IoFailed = "io_failed",
    /// The session stayed gone for 2 hours after a crash, so its keeper stopped the job.
    KeeperTimeout = "keeper_timeout",
    /// A log line that cannot be encoded, or does not parse.
    LogCorrupt = "log_corrupt",
    /// A login a person must finish; the message holds the URL and any device code.
    LoginUrl = "login_url",
    /// A command line Fiber could not read.
    Malformed = "malformed",
    /// A cancelled call the server may still act on.
    McpCancelRequested = "mcp_cancel_requested",
    /// Fiber could not get an MCP server's prompt.
    McpPromptFailed = "mcp_prompt_failed",
    /// A required MCP server failed to start.
    McpRequiredServerFailed = "mcp_required_server_failed",
    /// A repository's required MCP server is not approved.
    McpServerUnapproved = "mcp_server_unapproved",
    /// The server failed to start or died.
    McpServerUnavailable = "mcp_server_unavailable",
    /// The server has removed the tool.
    McpToolRemoved = "mcp_tool_removed",
    /// A `before_message` hook refused a message: a session message, or a person's or a driver's.
    MessageRefused = "message_refused",
    /// A bare model id matches models of two or more installed providers.
    ModelAmbiguous = "model_ambiguous",
    /// A model's `extra_body` names a field Fiber builds, or its
    /// `web_search` names a type its protocol does not read, so the model is
    /// left out of the model list.
    ModelInvalid = "model_invalid",
    /// A provider's discovery found a model and left it out on purpose;
    /// the message names the model and the provider's reason.
    ModelOmitted = "model_omitted",
    /// The provider does not know the model.
    ModelNotFound = "model_not_found",
    /// A model's base URL names a per-account host whose setting has no
    /// value, so the model is left out of the model list.
    ModelUnconfigured = "model_unconfigured",
    /// The terminal's model list is not read yet.
    ModelsLoading = "models_loading",
    /// `name_session` was called while the person's name pins the session.
    NamePinned = "name_pinned",
    /// `/jobs` ran in an attached session with no running jobs.
    NoJobsRunning = "no_jobs_running",
    /// An edit block's text was not found in the file.
    NoMatch = "no_match",
    /// Nothing chose a model.
    NoModel = "no_model",
    /// A process exited nonzero.
    NonzeroExit = "nonzero_exit",
    /// The path `read` or `edit` names does not exist.
    NotFound = "not_found",
    /// A rewind's `seq` is not a step boundary.
    NotStepBoundary = "not_step_boundary",
    /// A command arrived before `subscribe`.
    NotSubscribed = "not_subscribed",
    /// The job's keeper died before it wrote the job's end, or there was no keeper.
    Orphaned = "orphaned",
    /// A job's output file passed 5 GB.
    OutputCap = "output_cap",
    /// A reply was cut off by the output-token limit.
    OutputTruncated = "output_truncated",
    /// A pairing code was wrong, already used or too old.
    PairingFailed = "pairing_failed",
    /// A symbolic link changed between the permission decision and the write.
    PathChanged = "path_changed",
    /// A reply was still paused after 5 continuations in a row.
    PausedTooLong = "paused_too_long",
    /// A protected path changed while a confined call ran; startup
    /// recovery restored it.
    ProtectedPathsRestored = "protected_paths_restored",
    /// A provider server error or overload.
    ProviderUnavailable = "provider_unavailable",
    /// A quota, billing or subscription limit.
    QuotaExceeded = "quota_exceeded",
    /// The provider rate-limited the request.
    RateLimited = "rate_limited",
    /// The provider declined on policy grounds.
    Refused = "refused",
    /// Code a repository declares was skipped: nobody approved it and nobody
    /// could be asked.
    RepositoryCodeSkipped = "repository_code_skipped",
    /// The reviewer could not choose which of the person's messages still bind at a compaction, so it kept every one.
    ReviewerSelectionFailed = "reviewer_selection_failed",
    /// The reviewer blocked a call while both layers of the notes are empty;
    /// the notice names the `reviewer-setup` skill.
    ReviewerSetup = "reviewer_setup",
    /// `/sandbox` reports the session sandbox switch.
    Sandbox = "sandbox",
    /// A delete names a session that forks or rewinds point at.
    SessionHasDependents = "session_has_dependents",
    /// Another process holds the session.
    SessionHeld = "session_held",
    /// A resume names no session.
    SessionNotFound = "session_not_found",
    /// A process killed by a signal Fiber did not send.
    Signal = "signal",
    /// A skill's header does not parse or lacks `name` or `description`.
    SkillInvalid = "skill_invalid",
    /// Two skills share a name.
    SkillShadowed = "skill_shadowed",
    /// The skills listing passes 10% of the context window.
    SkillsLarge = "skills_large",
    /// A write would replace a file the session has not seen in its current
    /// state.
    StaleFile = "stale_file",
    /// What a command names is no longer pending or running.
    StaleRequest = "stale_request",
    /// A state value over 64 KiB.
    StateTooLarge = "state_too_large",
    /// The hub could not read the open session's log to serve this terminal.
    StreamClosed = "stream_closed",
    /// The stream ended early or carried an unmatched error.
    StreamIncomplete = "stream_incomplete",
    /// A `rewind` that asked for a summary could not get one.
    SummaryFailed = "summary_failed",
    /// A model or level choice sent while a turn runs: it applies when
    /// the turn ends, and the chips hold the old model and level until
    /// the session accepts it.
    SwitchPending = "switch_pending",
    /// A deadline passed.
    Timeout = "timeout",
    /// A `web_fetch` download larger than 10 MiB.
    TooLarge = "too_large",
    /// Full tool definitions take more than 10% of the context window.
    ToolDefinitionsLarge = "tool_definitions_large",
    /// The tool itself failed, or its effects function errored.
    ToolError = "tool_error",
    /// A remote hub connection presented no valid device token.
    Unauthenticated = "unauthenticated",
    /// A command name no extension registered.
    UnknownCommand = "unknown_command",
    /// A stop or finish reason Fiber does not map.
    UnknownStopReason = "unknown_stop_reason",
    /// The model named a tool that does not exist.
    UnknownTool = "unknown_tool",
    /// `session_message` named an id no running session has.
    Unreachable = "unreachable",
    /// A model replied, but not in the format Fiber asked for.
    UnreadableReply = "unreadable_reply",
    /// A file tool was given a directory, device or file it cannot handle.
    UnsupportedFile = "unsupported_file",
    /// The invocation or its environment is wrong; exits 2.
    Usage = "usage",
    /// An install needs two majors of one dependency, or no tag meets a
    /// minimum.
    VersionConflict = "version_conflict",
    /// `web_search` is not declared: several search backends are installed and
    /// none is chosen, or the chosen one is not installed.
    WebSearchUnavailable = "web_search_unavailable",
    /// `/pwd` shows the attached session's recorded workspace root.
    Workspace = "workspace",
}

#[cfg(test)]
#[path = "codes_tests.rs"]
mod tests;
