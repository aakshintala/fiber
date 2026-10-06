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
use crate::server::CallError;
use crate::slot::{self, Run, Slot};

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
    /// The server's slot. A dead link means the session is gone.
    slot: Weak<Slot>,
}

impl McpTool {
    /// Declares `tool` of `server`: the qualified name, the server's
    /// description and schema, and `hints` already resolved (the person's
    /// override replaces the server's set as a whole). Calls run through
    /// the server's slot, starting it on the first call.
    pub(crate) fn declare(
        server: &str,
        tool: &str,
        description: String,
        schema: Value,
        hints: &Hints,
        timeout: Duration,
        slot: Weak<Slot>,
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
                slot,
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
        let Some(slot) = self.call.slot.upgrade() else {
            return failed(
                ErrorCode::McpServerUnavailable,
                slot::unavailable(&self.call.server),
                None,
            );
        };
        match slot.run(&self.call.tool) {
            Run::Removed => failed(
                ErrorCode::McpToolRemoved,
                slot::removed(&self.call.server, &self.call.tool),
                None,
            ),
            Run::Failed(failed) => Output {
                error: Some(failed.error),
                server_failed: failed.record,
                ..Output::default()
            },
            Run::Call(server) => match server.call(
                &self.call.tool,
                &Value::Object(arguments.clone()),
                self.call.timeout,
                cancel,
            ) {
                Ok(result) => answer(&self.call.server, &self.call.tool, &result),
                Err(CallError::Timeout) => failed(ErrorCode::Timeout, timed_out(&self.call), None),
                Err(CallError::Cancelled) => {
                    failed(ErrorCode::McpCancelRequested, cancelled(&self.call), None)
                }
                Err(CallError::Gone) => failed(
                    ErrorCode::McpServerUnavailable,
                    slot::unavailable(&self.call.server),
                    None,
                ),
                Err(CallError::JsonRpc { code: _, message }) => {
                    failed(ErrorCode::ToolError, message, None)
                }
            },
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

fn failed(
    code: ErrorCode,
    message: String,
    server_failed: Option<contract::events::McpServerFailed>,
) -> Output {
    Output {
        error: Some(Failure {
            code,
            message,
            retry_after: None,
            provider: None,
        }),
        server_failed,
        ..Output::default()
    }
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
