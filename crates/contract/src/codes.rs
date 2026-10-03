//! The error codes (`docs/errors.md`), the notice codes and the driver command
//! rejection codes (`docs/invocation.md`, "Driver commands"). They share one
//! namespace: a label means the same thing wherever it appears.

use serde::{Deserialize, Serialize};

/// Declares [`ErrorCode`] and, for tests, the list of every known code.
macro_rules! codes {
    ($($(#[doc = $doc:literal])+ $name:ident,)+) => {
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

        #[cfg(test)]
        const KNOWN: &[ErrorCode] = &[$(ErrorCode::$name,)+];
    };
}

codes! {
    /// An edit block's text occurs more than once.
    AmbiguousMatch,
    /// The provider rejected the credential.
    AuthenticationFailed,
    /// The block budget ran out with no human to answer.
    Blocked,
    /// `web_fetch` named a link-local address or a cloud metadata host.
    BlockedHost,
    /// The spending budget was reached, or an extension refused a model
    /// request.
    BudgetExceeded,
    /// A turn is running.
    Busy,
    /// The session was sent `close`.
    Closing,
    /// Two extensions registered the same command name.
    CommandConflict,
    /// A configuration file is invalid.
    ConfigInvalid,
    /// An unknown key, or a key a repository may not set.
    ConfigKeyIgnored,
    /// The connection to the provider failed.
    ConnectionFailed,
    /// The request does not fit the context window.
    ContextOverflow,
    /// A stored credential cannot be used, or the provider's `credential()` or
    /// `sign()` failed.
    CredentialFailed,
    /// No credential was found for the session's model.
    CredentialMissing,
    /// A command named a delegate's session.
    DelegateSession,
    /// A delegate tool at depth 2.
    DepthExceeded,
    /// An extension failed to start or missed its deadline.
    ExtensionFailed,
    /// An extension needs a newer `fiber` or a different extension API version.
    ExtensionIncompatible,
    /// The session model's provider is not installed.
    ExtensionMissing,
    /// An install names a repository or tag that does not exist.
    ExtensionNotFound,
    /// A required extension failed to start.
    ExtensionRequiredFailed,
    /// The extension providing the tool died twice.
    ExtensionUnavailable,
    /// An install or update could not fetch: git or the network failed.
    FetchFailed,
    /// A monitor was suppressed for 30 seconds.
    Flooded,
    /// An extension hook errored or ran out of time.
    HookFailed,
    /// `web_fetch` got a status other than 2xx.
    HttpError,
    /// Fiber cannot tell whether the call completed.
    Indeterminate,
    /// The instruction text passes 10% of the context window.
    InstructionsLarge,
    /// Arguments failed their schema or checks.
    InvalidArguments,
    /// The provider rejected the request for any other reason.
    InvalidRequest,
    /// A filesystem failure; the message names the path.
    IoFailed,
    /// A log line that cannot be encoded, or does not parse.
    LogCorrupt,
    /// A command line Fiber could not read.
    Malformed,
    /// A cancelled call the server may still act on.
    McpCancelRequested,
    /// A required MCP server failed to start.
    McpRequiredServerFailed,
    /// A repository's MCP server is not approved.
    McpServerUnapproved,
    /// The server failed to start or died.
    McpServerUnavailable,
    /// The server has removed the tool.
    McpToolRemoved,
    /// The target session's `before_message` refused a session message.
    MessageRefused,
    /// A bare model id matches models of two or more installed providers.
    ModelAmbiguous,
    /// The provider does not know the model.
    ModelNotFound,
    /// `name_session` was called while the person's name pins the session.
    NamePinned,
    /// An edit block's text was not found in the file.
    NoMatch,
    /// Nothing chose a model.
    NoModel,
    /// A process exited nonzero.
    NonzeroExit,
    /// The path `read` or `edit` names does not exist.
    NotFound,
    /// A rewind's `seq` is not a step boundary.
    NotStepBoundary,
    /// A command arrived before `subscribe`.
    NotSubscribed,
    /// The process that ran the job died.
    Orphaned,
    /// A job's output file passed 5 GB.
    OutputCap,
    /// A reply was cut off by the output-token limit.
    OutputTruncated,
    /// A symbolic link changed between the permission decision and the write.
    PathChanged,
    /// The model's protocol is one this Fiber does not speak yet.
    ProtocolUnsupported,
    /// A provider server error or overload.
    ProviderUnavailable,
    /// A quota, billing or subscription limit.
    QuotaExceeded,
    /// The provider rate-limited the request.
    RateLimited,
    /// The provider declined on policy grounds.
    Refused,
    /// Another process holds the session.
    SessionHeld,
    /// A resume names no session.
    SessionNotFound,
    /// A process killed by a signal Fiber did not send.
    Signal,
    /// A write would replace a file the session has not seen in its current
    /// state.
    StaleFile,
    /// What a command names is no longer pending or running.
    StaleRequest,
    /// A state value over 64 KiB.
    StateTooLarge,
    /// The stream ended early or carried an unmatched error.
    StreamIncomplete,
    /// A deadline passed.
    Timeout,
    /// A `web_fetch` download larger than 10 MiB.
    TooLarge,
    /// Full tool definitions take more than 10% of the context window.
    ToolDefinitionsLarge,
    /// The tool itself failed, or its effects function errored.
    ToolError,
    /// A command name no extension registered.
    UnknownCommand,
    /// A stop or finish reason Fiber does not map.
    UnknownStopReason,
    /// The model named a tool that does not exist.
    UnknownTool,
    /// `session_message` named an id no running session has.
    Unreachable,
    /// A model replied, but not in the format Fiber asked for.
    UnreadableReply,
    /// A file tool was given a directory, device or file it cannot handle.
    UnsupportedFile,
    /// The invocation or its environment is wrong; exits 2.
    Usage,
    /// An install needs two majors of one dependency, or no tag meets a
    /// minimum.
    VersionConflict,
}

#[cfg(test)]
#[path = "codes_tests.rs"]
mod tests;
