//! Every MCP failure as its own case (`docs/errors.md`, "Where a code
//! comes from"): one fixed code per case, mapped in [`McpError::code`]
//! with no wildcard arm, so a new case does not compile until it has one.
//! Each case's message is the sentence shown for it, and the contract
//! records (`Output.error`, `McpServerFailed.error`) are built from a case
//! at the edge through `From<&McpError> for Failure`.

use contract::ErrorCode;
use contract::shapes::Failure;

/// Every way an MCP call, prompt, resource read or server start fails.
/// Cloned into `State::Dead`, so it stays comparable and shareable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum McpError {
    /// The server is gone: no spawn was tried, or its start failed, or it
    /// has since exited.
    #[error("The MCP server `{server}` did not start, or it has since exited.")]
    ServerUnavailable { server: String },
    /// The call saw the remote session expire: the next call starts a new
    /// session, with no record.
    #[error(
        "The MCP server `{server}` ended its session; Fiber starts a new one on the next call."
    )]
    SessionExpired { server: String },
    /// The server exited; the next call restarts it.
    #[error("The MCP server `{server}` exited; Fiber restarts it on the next call.")]
    ServerExited { server: String },
    /// The server exited again; its tools fail until the next session.
    #[error("The MCP server `{server}` exited again; its tools fail until the next session.")]
    ServerExitedAgain { server: String },
    /// A remote server stopped answering; Fiber reconnects on the next call.
    #[error("The MCP server `{server}` stopped answering; Fiber reconnects on the next call.")]
    ServerStoppedAnswering { server: String },
    /// A remote server stopped answering again; its tools fail until the
    /// next session.
    #[error(
        "The MCP server `{server}` stopped answering again; its tools fail until the next session."
    )]
    ServerStoppedAnsweringAgain { server: String },
    /// The spawn, `initialize` or `tools/list` failed. A remote server
    /// has no `command` to check: its advice names its `url`.
    #[error(
        "{} failed to start: {detail} {}",
        start_subject(.server, *.required),
        start_advice(*.remote)
    )]
    StartFailed {
        server: String,
        required: bool,
        remote: bool,
        detail: String,
    },
    /// The remote server needs a usable OAuth login.
    #[error(
        "{} failed to start: {}",
        start_subject(.server, *.required),
        login_sentence(.server, .detail)
    )]
    NotLoggedIn {
        server: String,
        required: bool,
        detail: String,
    },
    /// Nothing answered before the startup deadline.
    #[error(
        "{} did not answer before its startup deadline of {startup_ms} ms. \
         Raise `startup_timeout_ms` under `mcp.servers.{server}` if it needs longer.",
        start_subject(.server, *.required)
    )]
    StartDeadline {
        server: String,
        required: bool,
        startup_ms: u128,
    },
    /// A `{ "secret" }` env value names no stored credential.
    #[error(
        "{} names the secret `credentials/{name}`, which is not stored. \
         Run `fiber mcp remove {server}`, then `fiber mcp add {server}` \
         with `--secret {key}` to store it.",
        start_subject(.server, *.required),
        key = secret_key(.name)
    )]
    SecretMissing {
        server: String,
        required: bool,
        name: String,
    },
    /// A `{ "secret" }` env value names a credential that cannot be read.
    #[error(
        "{} names the secret `credentials/{name}`, which cannot be read: {why}. \
         Run `fiber mcp remove {server}`, then `fiber mcp add {server}` \
         with `--secret {key}` to store it again.",
        start_subject(.server, *.required),
        key = secret_key(.name)
    )]
    SecretUnreadable {
        server: String,
        required: bool,
        name: String,
        why: String,
    },
    /// Nothing answered before the call's deadline.
    #[error("The MCP server `{server}` did not answer `{tool}` within {timeout_ms} ms.")]
    CallTimedOut {
        server: String,
        tool: String,
        timeout_ms: u128,
    },
    /// The call was cancelled.
    #[error(
        "The call to `{tool}` on the MCP server `{server}` was cancelled; \
         the server may still act on it."
    )]
    CallCancelled { server: String, tool: String },
    /// The server refused the call, with its own sentence.
    #[error("{message}")]
    CallRefused { message: String },
    /// The running server no longer lists the tool: the call never
    /// reaches the server, and the tool stays declared until the next
    /// session.
    #[error(
        "The MCP server `{server}` no longer has the tool `{tool}`; \
         it stays declared until the next session."
    )]
    ToolRemoved { server: String, tool: String },
    /// A result the server marked as an error, with the message built
    /// from its text parts or the default sentence.
    #[error("{message}")]
    ToolReportedError { message: String },
    /// An image part with no data.
    #[error("image content: no data")]
    ImageNoData,
    /// An image part that is not a base64 string.
    #[error("image content: expected a base64 string")]
    ImageNotBase64String,
    /// An image part that does not decode.
    #[error("image content: {0}")]
    ImageUndecodable(#[source] base64::DecodeError),
    /// Decoded bytes the image child cannot read as an image.
    #[error("The image cannot be read as an image: {message}")]
    ImageUnreadable { message: String },
    /// Decoded bytes the image child refused, with its own sentence.
    #[error("{message}")]
    ImageFailed { message: String },
    /// An `mcp_resources` call whose named field is not a string.
    #[error("`{field}` must be a string.")]
    ResourceFieldNotString {
        /// The field that is not a string: `server`, `cursor` or `uri`.
        field: &'static str,
    },
    /// An `mcp_resources` `read` without both its `server` and its `uri`.
    #[error("`read` needs `server` and `uri`.")]
    ResourceReadNeedsServerAndUri,
    /// An `mcp_resources` call whose `action` is not `list` or `read`.
    #[error("`action` must be `list` or `read`.")]
    ResourceActionUnknown,
    /// The call names no server that offers resources.
    #[error(
        "No MCP server named `{server}` offers resources; \
         `list` with no `server` names those that do."
    )]
    NoResources { server: String },
    /// The server answered `resources/read` without its contents.
    #[error("The MCP server `{server}` answered `resources/read` without its contents.")]
    ResourceContentsMissing { server: String },
    /// The server answered `resources/list` without a resource list.
    #[error("The MCP server `{server}` answered `resources/list` without a resource list.")]
    ResourceListMissing { server: String },
    /// No prompt of that name on that server.
    #[error("The MCP server `{server}` has no prompt `/{prompt}`.")]
    PromptMissing { server: String, prompt: String },
    /// Words after `/name` left a required argument empty.
    #[error("The MCP server `{server}`'s prompt `/{prompt}` needs {needs}.{follow}")]
    PromptArgumentsMissing {
        server: String,
        prompt: String,
        needs: String,
        follow: String,
    },
    /// The prompt's text cannot be had for a server-side cause. Every
    /// cause the slot reports starts with "The ", so the cause reads as
    /// the sentence's end; one without stays as it is.
    #[error(
        "The MCP server `{server}`'s prompt `/{prompt}` was not run: the {}",
        without_the(&cause.to_string())
    )]
    PromptNotRun {
        server: String,
        prompt: String,
        #[source]
        cause: Box<McpError>,
    },
    /// The `prompts/get` answer did not parse as messages.
    #[error(
        "The MCP server `{server}`'s prompt `/{prompt}` returned no messages, \
         which Fiber cannot send as a message."
    )]
    PromptNoMessages { server: String, prompt: String },
    /// The `prompts/get` answer held nothing sendable as a message.
    #[error(
        "The MCP server `{server}`'s prompt `/{prompt}` returned {reason}, \
         which Fiber cannot send as a message."
    )]
    PromptUnreadable {
        server: String,
        prompt: String,
        reason: String,
    },
    /// Nothing answered the prompt before its timeout.
    #[error(
        "The MCP server `{server}` did not answer the prompt `/{prompt}` \
         within {timeout_ms} ms."
    )]
    PromptTimedOut {
        server: String,
        prompt: String,
        timeout_ms: u128,
    },
    /// The prompt run was cancelled.
    #[error(
        "The run of the prompt `/{prompt}` on the MCP server `{server}` was \
         cancelled; the server may still act on it."
    )]
    PromptCancelled { server: String, prompt: String },
    /// The server refused the prompt, with its own sentence.
    #[error("The MCP server `{server}` refused the prompt `/{prompt}`: {message}.")]
    PromptRefused {
        server: String,
        prompt: String,
        message: String,
    },
}

impl McpError {
    /// The code from `docs/errors.md`: one fixed code per case, with no
    /// wildcard arm.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Self::ServerUnavailable { .. }
            | Self::SessionExpired { .. }
            | Self::ServerExited { .. }
            | Self::ServerExitedAgain { .. }
            | Self::ServerStoppedAnswering { .. }
            | Self::ServerStoppedAnsweringAgain { .. }
            | Self::StartFailed { .. }
            | Self::NotLoggedIn { .. }
            | Self::StartDeadline { .. } => ErrorCode::McpServerUnavailable,
            Self::SecretMissing { .. } => ErrorCode::CredentialMissing,
            Self::SecretUnreadable { .. } => ErrorCode::CredentialFailed,
            Self::CallTimedOut { .. } => ErrorCode::Timeout,
            Self::CallCancelled { .. } => ErrorCode::McpCancelRequested,
            Self::CallRefused { .. }
            | Self::ToolReportedError { .. }
            | Self::ImageFailed { .. } => ErrorCode::ToolError,
            Self::ToolRemoved { .. } => ErrorCode::McpToolRemoved,
            Self::ImageNoData
            | Self::ImageNotBase64String
            | Self::ImageUndecodable(_)
            | Self::ImageUnreadable { .. } => ErrorCode::UnsupportedFile,
            Self::ResourceFieldNotString { .. }
            | Self::ResourceReadNeedsServerAndUri
            | Self::ResourceActionUnknown
            | Self::NoResources { .. } => ErrorCode::InvalidArguments,
            Self::ResourceContentsMissing { .. } | Self::ResourceListMissing { .. } => {
                ErrorCode::ToolError
            }
            Self::PromptMissing { .. }
            | Self::PromptNotRun { .. }
            | Self::PromptNoMessages { .. }
            | Self::PromptUnreadable { .. }
            | Self::PromptTimedOut { .. }
            | Self::PromptCancelled { .. }
            | Self::PromptRefused { .. } => ErrorCode::McpPromptFailed,
            Self::PromptArgumentsMissing { .. } => ErrorCode::InvalidArguments,
        }
    }
}

impl From<&McpError> for Failure {
    /// The contract record of a case: its code and its sentence, with no
    /// retry and no provider.
    fn from(error: &McpError) -> Failure {
        Failure {
            code: error.code(),
            message: error.to_string(),
            retry_after_ms: None,
            provider: None,
        }
    }
}

/// The start failure's subject: `required` servers name themselves,
/// because the session stops before the log.
fn start_subject(server: &str, required: bool) -> String {
    if required {
        format!("The required MCP server `{server}`")
    } else {
        format!("The MCP server `{server}`")
    }
}

/// The start failure's advice: a remote server has no `command` to check.
fn start_advice(remote: bool) -> &'static str {
    if remote {
        "Check its `url` under `mcp.servers` in your configuration."
    } else {
        "Check its `command` and `args` under `mcp.servers` in your configuration."
    }
}

/// The sentence a login failure carries: the transport's detail, then
/// the login to run. Shared by the start case and the mid-call refusal,
/// so the two cannot drift.
pub(crate) fn login_sentence(server: &str, detail: &str) -> String {
    format!("{detail} Run `fiber mcp login {server}` to log in.")
}

/// The `--secret` key of a stored name: what follows its last dot.
fn secret_key(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// The cause as the "was not run" sentence's end: without its leading
/// "The ", so the cause reads as the sentence's end.
fn without_the(cause: &str) -> String {
    cause.strip_prefix("The ").unwrap_or(cause).to_owned()
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
