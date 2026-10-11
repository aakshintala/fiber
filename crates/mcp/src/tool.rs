//! One MCP tool as a Fiber tool (`docs/mcp.md`, "Tools and their names"
//! and "Calls"): the qualified name, the server's description and schema,
//! effects from the resolved hints, and calls run on the server under the
//! spec's timeout. Definitions are never deferred. The loop applies the 16
//! KiB cut and writes the artifact, so [`Tool::bound`] stays default.

use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use contract::ErrorCode;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::ContentPart;
use contract::tool::{Cancel, Effects, Output, ServerRecord, Tool};
use serde_json::{Map, Value};

use crate::effects::Hints;
use crate::fail;
use crate::name::qualified;
use crate::rpc::{Named, TOOLS_CALL};
use crate::server_json::{CallResult, Content, Resource, from_object};
use crate::slot::{self, Fault, Run, Slot};

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
    /// The session artifacts dir: binary content is written here.
    artifacts: PathBuf,
    /// The server's slot. A dead link means the session is gone.
    slot: Weak<Slot>,
}

/// Where a declared tool's calls run: the call timeout, the session
/// artifacts dir for binary content, and the server's slot.
pub(crate) struct Target {
    /// The call timeout, from the spec.
    pub timeout: Duration,
    /// The session artifacts dir: binary content is written here.
    pub artifacts: PathBuf,
    /// The server's slot. A dead link means the session is gone.
    pub slot: Weak<Slot>,
}

impl McpTool {
    /// The server's own tool name, sent as `tools/call`'s `name`.
    pub(crate) fn tool_name(&self) -> &str {
        &self.call.tool
    }

    /// Declares `tool` of `server`: the qualified name, the server's
    /// description and schema, and `hints` already resolved (the person's
    /// override replaces the server's set as a whole). Calls run through
    /// the server's slot, starting it on the first call, and write binary
    /// content to the session artifacts dir.
    pub(crate) fn declare(
        server: &str,
        tool: &str,
        description: String,
        schema: Value,
        hints: &Hints,
        target: Target,
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
                always_reviewed: false,
            },
            call: Call {
                server: server.to_owned(),
                tool: tool.to_owned(),
                timeout: target.timeout,
                artifacts: target.artifacts,
                slot: target.slot,
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
            return fail::failed(
                ErrorCode::McpServerUnavailable,
                slot::unavailable(&self.call.server),
            );
        };
        match slot.run(&self.call.tool) {
            Run::Removed(servers) => Output {
                servers,
                ..fail::failed(
                    ErrorCode::McpToolRemoved,
                    slot::removed(&self.call.server, &self.call.tool),
                )
            },
            Run::Failed(failed) => Output {
                error: Some(failed.error),
                servers: failed.records,
                ..Output::default()
            },
            Run::Call(server, servers) => {
                let params = Named {
                    arguments,
                    name: &self.call.tool,
                };
                let called = slot.call(&server, TOOLS_CALL, &params, self.call.timeout, cancel);
                // The call is never replayed: the next call restarts
                // the server, if a restart is left.
                output(&self.call, called, servers)
            }
        }
    }
}

/// Maps one call outcome to its output.
fn output(call: &Call, called: Result<Value, Fault>, servers: Vec<ServerRecord>) -> Output {
    match called {
        Ok(result) => {
            let result: CallResult = from_object(result).unwrap_or_default();
            let mut answered = answer(call, &result);
            answered.servers = servers;
            answered
        }
        Err(Fault::Timeout) => {
            let mut out = fail::failed(ErrorCode::Timeout, timed_out(call));
            out.servers = servers;
            out
        }
        Err(Fault::Cancelled) => {
            let mut out = fail::failed(ErrorCode::McpCancelRequested, cancelled(call));
            out.servers = servers;
            out
        }
        Err(Fault::Refused(message)) => {
            let mut out = fail::failed(ErrorCode::ToolError, message);
            out.servers = servers;
            out
        }
        Err(Fault::Died(record)) => {
            let mut out = fail::failed(record.error.code.clone(), record.error.message.clone());
            let mut all = servers;
            all.push(ServerRecord::Failed(record));
            out.servers = all;
            out
        }
        Err(Fault::Gone) => {
            let mut out = fail::failed(
                ErrorCode::McpServerUnavailable,
                slot::unavailable(&call.server),
            );
            out.servers = servers;
            out
        }
    }
}

/// Reads a `tools/call` result: every part becomes its own text block in
/// order. Text stays text; a resource link becomes one line giving its
/// URI, name and MIME type; an embedded text resource becomes its text;
/// and binary content (a blob resource or audio data) is written to the
/// session artifacts dir, with one line giving its MIME type, size and
/// path. Images are dropped, and anything else becomes
/// `Unsupported content.`. A result the server marks as an error ends
/// failed with code `tool_error`, with the message built from its text
/// parts only. `structuredContent` is dropped in this slice.
fn answer(call: &Call, result: &CallResult) -> Output {
    let mut lines = Vec::new();
    let mut error_texts = Vec::new();
    for part in &result.content {
        match part {
            Content::Text { text: Some(text) } => {
                error_texts.push(text.clone());
                lines.push(text.clone());
            }
            Content::Text { text: None } => {}
            Content::ResourceLink {
                uri,
                name,
                mime_type,
            } => lines.push(resource_link_line(uri, name, mime_type)),
            Content::Resource { resource } => {
                lines.push(resource_line(&call.artifacts, resource));
            }
            Content::Audio { data, mime_type } => {
                if let Some(encoded) = data {
                    lines.push(saved_line(&call.artifacts, mime_type, encoded));
                }
            }
            Content::Image => {}
            Content::Other | Content::Unreadable => {
                lines.push("Unsupported content.".to_owned());
            }
        }
    }
    let text = error_texts.join("");
    let content = lines
        .into_iter()
        .map(|text| ContentPart::Text { text })
        .collect();
    if result.is_error {
        let message = if text.is_empty() {
            format!(
                "The MCP server `{}` reported an error for `{}`.",
                call.server, call.tool,
            )
        } else {
            text
        };
        Output {
            content,
            error: Some(fail::failure(ErrorCode::ToolError, message)),
            ..Output::default()
        }
    } else {
        Output {
            content,
            ..Output::default()
        }
    }
}

/// One line for a resource link: its URI, with the name and MIME type it
/// names in parentheses, leaving out what the server did not name. An
/// empty name reads as absent.
fn resource_link_line(
    uri: &Option<String>,
    name: &Option<String>,
    mime_type: &Option<String>,
) -> String {
    let Some(uri) = uri.as_deref() else {
        return "Resource link: (no uri)".to_owned();
    };
    let mut details = Vec::new();
    if let Some(name) = name.as_deref().filter(|name| !name.is_empty()) {
        details.push(format!("name: {name}"));
    }
    if let Some(mime) = mime_type.as_deref() {
        details.push(format!("type: {mime}"));
    }
    if details.is_empty() {
        format!("Resource link: {uri}")
    } else {
        format!("Resource link: {uri} ({})", details.join(", "))
    }
}

/// One line for an embedded resource: its text, or its blob saved to the
/// session artifacts dir. A resource with neither reads as unreadable.
fn resource_line(artifacts: &Path, resource: &Resource) -> String {
    match (&resource.text, &resource.blob) {
        (Some(text), _) => text.clone(),
        (None, Some(blob)) => saved_line(artifacts, &resource.mime_type, blob),
        (None, None) => "An unreadable resource.".to_owned(),
    }
}

/// The default MIME type, when the server names none.
const DEFAULT_MIME_TYPE: &str = "application/octet-stream";

/// Saves base64 `encoded` to the session artifacts dir under a
/// content-addressed name: one line giving its MIME type, size and path.
/// Anything that cannot be saved becomes a `Could not save` line instead,
/// and the call does not fail.
fn saved_line(artifacts: &Path, mime_type: &Option<String>, encoded: &Value) -> String {
    let mime = mime_type.as_deref().unwrap_or(DEFAULT_MIME_TYPE);
    let Some(text) = encoded.as_str() else {
        return format!("Could not save {mime} content: expected a base64 string");
    };
    let bytes = match STANDARD.decode(text) {
        Ok(bytes) => bytes,
        Err(error) => return format!("Could not save {mime} content: {error}"),
    };
    let name = artifact_name(&bytes);
    match std::fs::write(artifacts.join(&name), &bytes) {
        Ok(()) => format!(
            "Saved {mime} content, {} bytes, to artifacts/{name}",
            bytes.len(),
        ),
        Err(error) => format!("Could not save {mime} content: {error}"),
    }
}

/// The content-addressed file name for `bytes`: the first 16 hex digits
/// of their SHA-256, so a repeated result rewrites the same file.
fn artifact_name(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let hex: String = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("mcp-{}.bin", &hex[..16])
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

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
