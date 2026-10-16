//! One row per [`McpError`](super::McpError) case: its code and its
//! sentence. Two rows per case that branches, so a changed code or a
//! changed word fails here.

use base64::Engine as _;
use contract::ErrorCode;
use contract::shapes::Failure;

use super::McpError;

struct Row {
    name: &'static str,
    error: McpError,
    code: ErrorCode,
    message: &'static str,
}

fn undecodable() -> McpError {
    let source = base64::engine::general_purpose::STANDARD
        .decode("!!!")
        .expect_err("three bangs do not decode");
    McpError::ImageUndecodable(source)
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            name: "unavailable",
            error: McpError::ServerUnavailable {
                server: "fx".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The MCP server `fx` did not start, or it has since exited.",
        },
        Row {
            name: "expired",
            error: McpError::SessionExpired {
                server: "fx".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The MCP server `fx` ended its session; Fiber starts a new one on the next call.",
        },
        Row {
            name: "exited",
            error: McpError::ServerExited {
                server: "fx".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The MCP server `fx` exited; Fiber restarts it on the next call.",
        },
        Row {
            name: "exited again",
            error: McpError::ServerExitedAgain {
                server: "fx".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The MCP server `fx` exited again; its tools fail until the next session.",
        },
        Row {
            name: "stopped answering",
            error: McpError::ServerStoppedAnswering {
                server: "fx".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The MCP server `fx` stopped answering; Fiber reconnects on the next call.",
        },
        Row {
            name: "stopped answering again",
            error: McpError::ServerStoppedAnsweringAgain {
                server: "fx".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The MCP server `fx` stopped answering again; its tools fail until the next session.",
        },
        Row {
            name: "start failed, optional stdio",
            error: McpError::StartFailed {
                server: "docs".to_owned(),
                required: false,
                remote: false,
                detail: "boom".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The MCP server `docs` failed to start: boom Check its `command` and `args` \
                      under `mcp.servers` in your configuration.",
        },
        Row {
            name: "start failed, required stdio",
            error: McpError::StartFailed {
                server: "docs".to_owned(),
                required: true,
                remote: false,
                detail: "boom".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The required MCP server `docs` failed to start: boom Check its `command` \
                      and `args` under `mcp.servers` in your configuration.",
        },
        Row {
            name: "start failed, optional remote",
            error: McpError::StartFailed {
                server: "docs".to_owned(),
                required: false,
                remote: true,
                detail: "boom".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The MCP server `docs` failed to start: boom Check its `url` under \
                      `mcp.servers` in your configuration.",
        },
        Row {
            name: "start failed, required remote",
            error: McpError::StartFailed {
                server: "docs".to_owned(),
                required: true,
                remote: true,
                detail: "boom".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The required MCP server `docs` failed to start: boom Check its `url` \
                      under `mcp.servers` in your configuration.",
        },
        Row {
            name: "not logged in, optional",
            error: McpError::NotLoggedIn {
                server: "docs".to_owned(),
                required: false,
                detail: "detail".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The MCP server `docs` failed to start: detail Run `fiber mcp login docs` \
                      to log in.",
        },
        Row {
            name: "not logged in, required",
            error: McpError::NotLoggedIn {
                server: "docs".to_owned(),
                required: true,
                detail: "detail".to_owned(),
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The required MCP server `docs` failed to start: detail Run \
                      `fiber mcp login docs` to log in.",
        },
        Row {
            name: "deadline, optional",
            error: McpError::StartDeadline {
                server: "docs".to_owned(),
                required: false,
                startup_ms: 5000,
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The MCP server `docs` did not answer before its startup deadline of 5000 \
                      ms. Raise `startup_timeout_ms` under `mcp.servers.docs` if it needs longer.",
        },
        Row {
            name: "deadline, required",
            error: McpError::StartDeadline {
                server: "docs".to_owned(),
                required: true,
                startup_ms: 5000,
            },
            code: ErrorCode::McpServerUnavailable,
            message: "The required MCP server `docs` did not answer before its startup deadline \
                      of 5000 ms. Raise `startup_timeout_ms` under `mcp.servers.docs` if it needs \
                      longer.",
        },
        Row {
            name: "secret missing, optional",
            error: McpError::SecretMissing {
                server: "docs".to_owned(),
                required: false,
                name: "mcp.docs.TOKEN".to_owned(),
            },
            code: ErrorCode::CredentialMissing,
            message: "The MCP server `docs` names the secret `credentials/mcp.docs.TOKEN`, which \
                      is not stored. Run `fiber mcp remove docs`, then `fiber mcp add docs` with \
                      `--secret TOKEN` to store it.",
        },
        Row {
            name: "secret missing, required",
            error: McpError::SecretMissing {
                server: "docs".to_owned(),
                required: true,
                name: "mcp.docs.TOKEN".to_owned(),
            },
            code: ErrorCode::CredentialMissing,
            message: "The required MCP server `docs` names the secret \
                      `credentials/mcp.docs.TOKEN`, which is not stored. Run `fiber mcp remove \
                      docs`, then `fiber mcp add docs` with `--secret TOKEN` to store it.",
        },
        Row {
            name: "secret missing, dotted name keeps its tail",
            error: McpError::SecretMissing {
                server: "docs".to_owned(),
                required: false,
                name: "TOKEN".to_owned(),
            },
            code: ErrorCode::CredentialMissing,
            message: "The MCP server `docs` names the secret `credentials/TOKEN`, which is not \
                      stored. Run `fiber mcp remove docs`, then `fiber mcp add docs` with \
                      `--secret TOKEN` to store it.",
        },
        Row {
            name: "secret unreadable, optional",
            error: McpError::SecretUnreadable {
                server: "docs".to_owned(),
                required: false,
                name: "mcp.docs.TOKEN".to_owned(),
                why: "denied".to_owned(),
            },
            code: ErrorCode::CredentialFailed,
            message: "The MCP server `docs` names the secret `credentials/mcp.docs.TOKEN`, which \
                      cannot be read: denied. Run `fiber mcp remove docs`, then `fiber mcp add \
                      docs` with `--secret TOKEN` to store it again.",
        },
        Row {
            name: "secret unreadable, required",
            error: McpError::SecretUnreadable {
                server: "docs".to_owned(),
                required: true,
                name: "mcp.docs.TOKEN".to_owned(),
                why: "denied".to_owned(),
            },
            code: ErrorCode::CredentialFailed,
            message: "The required MCP server `docs` names the secret \
                      `credentials/mcp.docs.TOKEN`, which cannot be read: denied. Run `fiber mcp \
                      remove docs`, then `fiber mcp add docs` with `--secret TOKEN` to store it \
                      again.",
        },
        Row {
            name: "call timed out",
            error: McpError::CallTimedOut {
                server: "fx".to_owned(),
                tool: "echo".to_owned(),
                timeout_ms: 60000,
            },
            code: ErrorCode::Timeout,
            message: "The MCP server `fx` did not answer `echo` within 60000 ms.",
        },
        Row {
            name: "call cancelled",
            error: McpError::CallCancelled {
                server: "fx".to_owned(),
                tool: "echo".to_owned(),
            },
            code: ErrorCode::McpCancelRequested,
            message: "The call to `echo` on the MCP server `fx` was cancelled; the server may \
                      still act on it.",
        },
        Row {
            name: "call refused",
            error: McpError::CallRefused {
                message: "Unknown tool: echo.".to_owned(),
            },
            code: ErrorCode::ToolError,
            message: "Unknown tool: echo.",
        },
        Row {
            name: "tool removed",
            error: McpError::ToolRemoved {
                server: "fx".to_owned(),
                tool: "echo".to_owned(),
            },
            code: ErrorCode::McpToolRemoved,
            message: "The MCP server `fx` no longer has the tool `echo`; it stays declared until \
                      the next session.",
        },
        Row {
            name: "tool reported an error",
            error: McpError::ToolReportedError {
                message: "The MCP server `fx` reported an error for `echo`.".to_owned(),
            },
            code: ErrorCode::ToolError,
            message: "The MCP server `fx` reported an error for `echo`.",
        },
        Row {
            name: "image with no data",
            error: McpError::ImageNoData,
            code: ErrorCode::UnsupportedFile,
            message: "image content: no data",
        },
        Row {
            name: "image that is not a string",
            error: McpError::ImageNotBase64String,
            code: ErrorCode::UnsupportedFile,
            message: "image content: expected a base64 string",
        },
        Row {
            name: "image that does not decode",
            error: undecodable(),
            code: ErrorCode::UnsupportedFile,
            message: "image content: Invalid symbol 33, offset 0.",
        },
        Row {
            name: "image the child cannot read",
            error: McpError::ImageUnreadable {
                message: "too small".to_owned(),
            },
            code: ErrorCode::UnsupportedFile,
            message: "The image cannot be read as an image: too small",
        },
        Row {
            name: "image the child refused",
            error: McpError::ImageFailed {
                message: "The server answered HTTP 500.".to_owned(),
            },
            code: ErrorCode::ToolError,
            message: "The server answered HTTP 500.",
        },
        Row {
            name: "request without an action",
            error: McpError::ResourceRequestInvalid {
                message: "`action` must be `list` or `read`.".to_owned(),
            },
            code: ErrorCode::InvalidArguments,
            message: "`action` must be `list` or `read`.",
        },
        Row {
            name: "request with a non-string server",
            error: McpError::ResourceRequestInvalid {
                message: "`server` must be a string.".to_owned(),
            },
            code: ErrorCode::InvalidArguments,
            message: "`server` must be a string.",
        },
        Row {
            name: "request with a non-string cursor",
            error: McpError::ResourceRequestInvalid {
                message: "`cursor` must be a string.".to_owned(),
            },
            code: ErrorCode::InvalidArguments,
            message: "`cursor` must be a string.",
        },
        Row {
            name: "read without its server and uri",
            error: McpError::ResourceRequestInvalid {
                message: "`read` needs `server` and `uri`.".to_owned(),
            },
            code: ErrorCode::InvalidArguments,
            message: "`read` needs `server` and `uri`.",
        },
        Row {
            name: "read with a non-string uri",
            error: McpError::ResourceRequestInvalid {
                message: "`uri` must be a string.".to_owned(),
            },
            code: ErrorCode::InvalidArguments,
            message: "`uri` must be a string.",
        },
        Row {
            name: "no resources",
            error: McpError::NoResources {
                server: "plain".to_owned(),
            },
            code: ErrorCode::InvalidArguments,
            message: "No MCP server named `plain` offers resources; `list` with no `server` \
                      names those that do.",
        },
        Row {
            name: "contents missing",
            error: McpError::ResourceContentsMissing {
                server: "docs".to_owned(),
            },
            code: ErrorCode::ToolError,
            message: "The MCP server `docs` answered `resources/read` without its contents.",
        },
        Row {
            name: "list missing",
            error: McpError::ResourceListMissing {
                server: "docs".to_owned(),
            },
            code: ErrorCode::ToolError,
            message: "The MCP server `docs` answered `resources/list` without a resource list.",
        },
        Row {
            name: "prompt missing",
            error: McpError::PromptMissing {
                server: "fx".to_owned(),
                prompt: "greet".to_owned(),
            },
            code: ErrorCode::McpPromptFailed,
            message: "The MCP server `fx` has no prompt `/greet`.",
        },
        Row {
            name: "prompt arguments missing, with a hint",
            error: McpError::PromptArgumentsMissing {
                server: "fx".to_owned(),
                prompt: "greet".to_owned(),
                needs: "<who>, <tone>".to_owned(),
                follow: " Run it as `/greet <who> [tone]`.".to_owned(),
            },
            code: ErrorCode::InvalidArguments,
            message: "The MCP server `fx`'s prompt `/greet` needs <who>, <tone>. Run it as \
                      `/greet <who> [tone]`.",
        },
        Row {
            name: "prompt arguments missing, with no hint",
            error: McpError::PromptArgumentsMissing {
                server: "fx".to_owned(),
                prompt: "greet".to_owned(),
                needs: "<who>".to_owned(),
                follow: String::new(),
            },
            code: ErrorCode::InvalidArguments,
            message: "The MCP server `fx`'s prompt `/greet` needs <who>.",
        },
        Row {
            name: "prompt not run, stripping the cause's The",
            error: McpError::PromptNotRun {
                server: "fx".to_owned(),
                prompt: "greet".to_owned(),
                cause: Box::new(McpError::ServerUnavailable {
                    server: "fx".to_owned(),
                }),
            },
            code: ErrorCode::McpPromptFailed,
            message: "The MCP server `fx`'s prompt `/greet` was not run: the MCP server `fx` did \
                      not start, or it has since exited.",
        },
        Row {
            name: "prompt not run, keeping a cause without The",
            error: McpError::PromptNotRun {
                server: "fx".to_owned(),
                prompt: "greet".to_owned(),
                cause: Box::new(McpError::CallRefused {
                    message: "Unknown tool: echo.".to_owned(),
                }),
            },
            code: ErrorCode::McpPromptFailed,
            message: "The MCP server `fx`'s prompt `/greet` was not run: the Unknown tool: echo.",
        },
        Row {
            name: "prompt with no messages",
            error: McpError::PromptNoMessages {
                server: "fx".to_owned(),
                prompt: "greet".to_owned(),
            },
            code: ErrorCode::McpPromptFailed,
            message: "The MCP server `fx`'s prompt `/greet` returned no messages, which Fiber \
                      cannot send as a message.",
        },
        Row {
            name: "prompt with an image",
            error: McpError::PromptUnreadable {
                server: "fx".to_owned(),
                prompt: "greet".to_owned(),
                reason: "an image".to_owned(),
            },
            code: ErrorCode::McpPromptFailed,
            message: "The MCP server `fx`'s prompt `/greet` returned an image, which Fiber \
                      cannot send as a message.",
        },
        Row {
            name: "prompt timed out",
            error: McpError::PromptTimedOut {
                server: "fx".to_owned(),
                prompt: "greet".to_owned(),
                timeout_ms: 60000,
            },
            code: ErrorCode::McpPromptFailed,
            message: "The MCP server `fx` did not answer the prompt `/greet` within 60000 ms.",
        },
        Row {
            name: "prompt cancelled",
            error: McpError::PromptCancelled {
                server: "fx".to_owned(),
                prompt: "greet".to_owned(),
            },
            code: ErrorCode::McpPromptFailed,
            message: "The run of the prompt `/greet` on the MCP server `fx` was cancelled; the \
                      server may still act on it.",
        },
        Row {
            name: "prompt refused",
            error: McpError::PromptRefused {
                server: "fx".to_owned(),
                prompt: "greet".to_owned(),
                message: "Unknown prompt.".to_owned(),
            },
            code: ErrorCode::McpPromptFailed,
            message: "The MCP server `fx` refused the prompt `/greet`: Unknown prompt..",
        },
    ]
}

#[test]
fn each_case_maps_to_its_code_sentence_and_record() {
    for row in rows() {
        assert_eq!(row.error.code(), row.code, "code, row: {}", row.name);
        assert_eq!(
            row.error.to_string(),
            row.message,
            "message, row: {}",
            row.name
        );
        assert_eq!(row.error.clone(), row.error, "clone, row: {}", row.name);
        let record = Failure::from(&row.error);
        assert_eq!(record.code, row.code, "record code, row: {}", row.name);
        assert_eq!(
            record.message, row.message,
            "record message, row: {}",
            row.name
        );
        assert_eq!(
            record.retry_after_ms, None,
            "record retry, row: {}",
            row.name
        );
        assert_eq!(record.provider, None, "record provider, row: {}", row.name);
        let owned = Failure::from(row.error.clone());
        assert_eq!(owned, record, "owned record, row: {}", row.name);
    }
}
