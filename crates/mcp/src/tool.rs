//! One MCP tool as a Fiber tool (`docs/mcp.md`, "Tools and their names"
//! and "Calls"): the qualified name, the server's description and schema,
//! effects from the resolved hints, and calls run on the server under the
//! spec's timeout. Definitions are never deferred. The loop applies the 16
//! KiB cut and writes the artifact, so [`Tool::bound`] stays default.

use std::sync::Weak;
use std::time::Duration;

use contract::ErrorCode;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Failure};
use contract::tool::{Cancel, Effects, Output, Tool};
use serde_json::{Map, Value};

use crate::effects::Hints;
use crate::name::qualified;
use crate::server::{CallError, Server};

/// One server tool declared to the model.
pub(crate) struct McpTool {
    definition: ToolDefinition,
    effects: Effects,
    call: Call,
}

struct Call {
    /// The server's configured name, for messages.
    server: String,
    /// The server's own tool name, sent as `tools/call`'s `name`.
    tool: String,
    /// The call timeout, from the spec.
    timeout: Duration,
    /// The shared connection. A dead link means the server is gone.
    link: Weak<Server>,
}

impl McpTool {
    /// Declares `tool` of `server`: the qualified name, the server's
    /// description and schema, and `hints` already resolved (the person's
    /// override replaces the server's set as a whole).
    pub(crate) fn declare(
        server: &str,
        tool: &str,
        description: String,
        schema: Value,
        hints: &Hints,
        timeout: Duration,
        link: Weak<Server>,
    ) -> Self {
        let name = qualified(server, tool);
        Self {
            definition: ToolDefinition {
                name,
                description,
                input_schema: schema,
                deferred: false,
                hosted: None,
            },
            effects: Effects {
                declared: hints.declared(),
                subject: Some(String::new()),
                prefix: None,
            },
            call: Call {
                server: server.to_owned(),
                tool: tool.to_owned(),
                timeout,
                link,
            },
        }
    }
}

impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn effects(
        &self,
        _arguments: &Map<String, Value>,
    ) -> Result<Effects, contract::tool::EffectsError> {
        // MCP hints are per tool, not per call (`docs/mcp.md`, "Effects"):
        // every call to one tool declares the same effects.
        Ok(self.effects.clone())
    }

    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        let Some(server) = self.call.link.upgrade() else {
            return failed(
                ErrorCode::McpServerUnavailable,
                unavailable(&self.call.server),
            );
        };
        match server.call(
            &self.call.tool,
            &Value::Object(arguments.clone()),
            self.call.timeout,
            cancel,
        ) {
            Ok(result) => answer(&self.call.server, &self.call.tool, &result),
            Err(CallError::Timeout) => failed(ErrorCode::Timeout, timed_out(&self.call)),
            Err(CallError::Cancelled) => {
                failed(ErrorCode::McpCancelRequested, cancelled(&self.call))
            }
            Err(CallError::Gone) => failed(
                ErrorCode::McpServerUnavailable,
                unavailable(&self.call.server),
            ),
            Err(CallError::JsonRpc { code: _, message }) => failed(ErrorCode::ToolError, message),
        }
    }
}

/// Reads a `tools/call` result: text blocks become content in order, and a
/// result the server marks as an error ends failed with code `tool_error`.
/// Non-text parts and `structuredContent` are dropped in this slice.
fn answer(server: &str, tool: &str, result: &Value) -> Output {
    let texts: Vec<String> = result
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| {
                    if part.get("type").and_then(Value::as_str) == Some("text") {
                        part.get("text").and_then(Value::as_str).map(str::to_owned)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let text = texts.join("");
    let content = texts
        .into_iter()
        .map(|text| ContentPart::Text { text })
        .collect();
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        let message = if text.is_empty() {
            format!("The MCP server `{server}` reported an error for `{tool}`.")
        } else {
            text
        };
        Output {
            content,
            error: Some(Failure {
                code: ErrorCode::ToolError,
                message,
                retry_after: None,
                provider: None,
            }),
            ..Output::default()
        }
    } else {
        Output {
            content,
            ..Output::default()
        }
    }
}

fn timed_out(call: &Call) -> String {
    format!(
        "The MCP server `{}` did not answer `{}` within {} ms.",
        call.server,
        call.tool,
        call.timeout.as_millis(),
    )
}

fn cancelled(call: &Call) -> String {
    format!(
        "The call to `{}` on the MCP server `{}` was cancelled; the server may still act on it.",
        call.tool, call.server,
    )
}

fn unavailable(server: &str) -> String {
    format!("The MCP server `{server}` did not start, or it has since exited.")
}

fn failed(code: ErrorCode, message: String) -> Output {
    Output {
        error: Some(Failure {
            code,
            message,
            retry_after: None,
            provider: None,
        }),
        ..Output::default()
    }
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
