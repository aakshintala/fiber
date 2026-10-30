//! One MCP tool as a Fiber tool (`docs/mcp.md`, "Tools and their names"
//! and "Calls"): the qualified name, the server's description and schema,
//! effects from the resolved hints, and calls run on the server under the
//! spec's timeout. Definitions are deferred by default: the loop declares
//! them with `defer_loading` on a model whose provider defers, unless the
//! configuration says otherwise. The loop applies the 16
//! KiB cut and writes the artifact, so [`Tool::bound`] stays default.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use contract::emit::Emit;
use contract::images::{ImageError, Images};
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, Failure};
use contract::tool::{Cancel, Effects, Output, ServerRecord, Tool};
use serde_json::{Map, Value};

use crate::effects::Hints;
use crate::elicit::Asker;
use crate::error::McpError;
use crate::fail;
use crate::name::qualified;
use crate::rpc::{Named, TOOLS_CALL};
use crate::server_json::{CallResult, Content, Resource, from_object};
use crate::slot::{Fault, Run, Slot};

/// One server tool declared to the model.
pub(crate) struct McpTool {
    definition: ToolDefinition,
    effects: Effects,
    call: Call,
}

pub(crate) struct Call {
    /// The server's configured name, for messages.
    server: String,
    /// The server's own tool name, sent as `tools/call`'s `name`.
    tool: String,
    /// The call timeout, from the spec.
    timeout: Duration,
    /// The session artifacts dir: binary content is written here.
    artifacts: PathBuf,
    /// The image child's driver: an image is processed once through it.
    images: Arc<dyn Images>,
    /// The server's slot. A dead link means the session is gone.
    slot: Weak<Slot>,
}

impl Call {
    /// One call to `tool` on `server`, running through `target`.
    pub(crate) fn new(server: &str, tool: &str, target: Target) -> Call {
        Call {
            server: server.to_owned(),
            tool: tool.to_owned(),
            timeout: target.timeout,
            artifacts: target.artifacts,
            images: target.images,
            slot: target.slot,
        }
    }
}

/// Where a declared tool's calls run: the call timeout, the session
/// artifacts dir for binary content, and the server's slot.
pub(crate) struct Target {
    /// The call timeout, from the spec.
    pub timeout: Duration,
    /// The session artifacts dir: binary content is written here.
    pub artifacts: PathBuf,
    /// The image child's driver: an image is processed once through it.
    pub images: Arc<dyn Images>,
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
        remote: bool,
        target: Target,
    ) -> Self {
        let name = qualified(server, tool);
        Self {
            definition: ToolDefinition {
                name,
                description,
                input_schema: schema,
                deferred: true,
                hosted: None,
            },
            effects: Effects {
                declared: hints.declared_for(remote),
                subject: Some(String::new()),
                prefix: None,
                always_reviewed: false,
                review_when_confined: false,
                hosts: None,
            },
            call: Call::new(server, tool, target),
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
        self.run_with(arguments, cancel, None)
    }

    fn run_asking(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        _emit: &dyn Emit,
        ask: &dyn contract::tool::Ask,
    ) -> Output {
        // The server name prompts carry is the configured one the call holds.
        self.run_with(
            arguments,
            cancel,
            Some(Asker {
                ask,
                server: &self.call.server,
            }),
        )
    }
}

impl McpTool {
    /// Runs the call, asking elicitations through `asker` when it is
    /// `Some`. `run` passes `None`, so `Server::call` keeps a production
    /// caller; `run_asking` passes `Some`.
    fn run_with(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        asker: Option<Asker<'_>>,
    ) -> Output {
        let Some(slot) = self.call.slot.upgrade() else {
            return fail::failed(&McpError::ServerUnavailable {
                server: self.call.server.clone(),
            });
        };
        match slot.run(&self.call.tool) {
            Run::Removed(servers) => Output {
                servers,
                ..fail::failed(&McpError::ToolRemoved {
                    server: self.call.server.clone(),
                    tool: self.call.tool.clone(),
                })
            },
            Run::Failed(failed) => Output {
                error: Some(Failure::from(&failed.error)),
                servers: failed.records,
                ..Output::default()
            },
            Run::Call(server, servers) => {
                let params = Named {
                    arguments,
                    name: &self.call.tool,
                };
                let called = match asker {
                    None => slot.call(&server, TOOLS_CALL, &params, self.call.timeout, cancel),
                    Some(asker) => slot.call_asking(
                        &server,
                        TOOLS_CALL,
                        &params,
                        self.call.timeout,
                        cancel,
                        asker,
                    ),
                };
                // The call is never replayed: the next call restarts
                // the server, if a restart is left.
                output(&self.call, called, servers, cancel)
            }
        }
    }
}

/// Maps one call outcome to its output.
fn output(
    call: &Call,
    called: Result<Value, Fault>,
    servers: Vec<ServerRecord>,
    cancel: &dyn Cancel,
) -> Output {
    match called {
        Ok(result) => {
            let result: CallResult = from_object(result).unwrap_or_default();
            let mut answered = answer(call, &result, cancel);
            answered.servers = servers;
            answered
        }
        Err(fault) => faulted(call, fault, servers),
    }
}

/// Maps one failed call to its output: the incoming records come first,
/// and a death appends its own record after them.
pub(crate) fn faulted(call: &Call, fault: Fault, servers: Vec<ServerRecord>) -> Output {
    match fault {
        Fault::Timeout => {
            let mut out = fail::failed(&McpError::CallTimedOut {
                server: call.server.clone(),
                tool: call.tool.clone(),
                timeout_ms: call.timeout.as_millis(),
            });
            out.servers = servers;
            out
        }
        Fault::Cancelled => {
            let mut out = fail::failed(&McpError::CallCancelled {
                server: call.server.clone(),
                tool: call.tool.clone(),
            });
            out.servers = servers;
            out
        }
        Fault::Refused(message) => {
            let mut out = fail::failed(&McpError::CallRefused { message });
            out.servers = servers;
            out
        }
        Fault::Expired => {
            let mut out = fail::failed(&McpError::SessionExpired {
                server: call.server.clone(),
            });
            out.servers = servers;
            out
        }
        Fault::Died(death) => {
            let mut out = fail::failed(&death.error);
            let mut all = servers;
            all.push(ServerRecord::Failed(death.record));
            out.servers = all;
            out
        }
        Fault::Gone => {
            let mut out = fail::failed(&McpError::ServerUnavailable {
                server: call.server.clone(),
            });
            out.servers = servers;
            out
        }
    }
}

/// Reads a `tools/call` result: every part becomes content in server
/// order. Text stays text; a resource link becomes one line giving its
/// URI, name and MIME type; an embedded text resource becomes its text;
/// and binary content (a blob resource or audio data) is written to the
/// session artifacts dir, with one line giving its MIME type, size and
/// path. An image is processed once through the image child and becomes
/// a text line and an image part; an embedded image resource takes the
/// same path. An image that cannot be processed ends the call failed,
/// and anything else becomes `Unsupported content.`. A result the server
/// marks as an error ends failed with code `tool_error`, with the message
/// built from its text parts only. `structuredContent` is dropped in this
/// slice.
pub(crate) fn answer(call: &Call, result: &CallResult, cancel: &dyn Cancel) -> Output {
    let mut content: Vec<ContentPart> = Vec::new();
    let mut error_texts = Vec::new();
    for part in &result.content {
        match part {
            Content::Text { text: Some(text) } => {
                error_texts.push(text.clone());
                content.push(ContentPart::Text { text: text.clone() });
            }
            Content::Text { text: None } => {}
            Content::ResourceLink {
                uri,
                name,
                mime_type,
            } => content.push(ContentPart::Text {
                text: resource_link_line(uri, name, mime_type),
            }),
            Content::Resource { resource } => match resource_text(call, resource, cancel) {
                Ok(mut parts) => content.append(&mut parts),
                Err(error) => return fail::failed(&error),
            },
            Content::Audio { data, mime_type } => {
                content.push(ContentPart::Text {
                    text: match data {
                        Some(encoded) => saved_line(&call.artifacts, mime_type, encoded),
                        None => format!(
                            "Could not save {} content: no data",
                            mime_type.as_deref().unwrap_or(DEFAULT_MIME_TYPE),
                        ),
                    },
                });
            }
            Content::Image { data, .. } => match image_content(call, data.as_ref(), cancel) {
                Ok(mut parts) => content.append(&mut parts),
                Err(error) => return fail::failed(&error),
            },
            Content::Other | Content::Unreadable => {
                content.push(ContentPart::Text {
                    text: "Unsupported content.".to_owned(),
                });
            }
        }
    }
    let text = error_texts.join("");
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
            error: Some(Failure::from(&McpError::ToolReportedError { message })),
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

/// One embedded resource's content: its text, an embedded image processed
/// once through the image child, or its blob saved to the session artifacts
/// dir. A resource with neither reads as unreadable. An image failure ends
/// the call failed, with no content.
fn resource_text(
    call: &Call,
    resource: &Resource,
    cancel: &dyn Cancel,
) -> Result<Vec<ContentPart>, McpError> {
    match (&resource.text, &resource.blob) {
        (Some(text), _) => Ok(vec![ContentPart::Text { text: text.clone() }]),
        (None, Some(blob)) if is_image(&resource.mime_type) => {
            image_content(call, Some(blob), cancel)
        }
        (None, Some(blob)) => Ok(vec![ContentPart::Text {
            text: saved_line(&call.artifacts, &resource.mime_type, blob),
        }]),
        (None, None) => Ok(vec![ContentPart::Text {
            text: "An unreadable resource.".to_owned(),
        }]),
    }
}

/// Whether the resource's MIME type names an image: `image/*` takes the
/// path `read` takes for an image, while anything else (a PDF included)
/// is saved to the session artifacts dir.
fn is_image(mime_type: &Option<String>) -> bool {
    mime_type
        .as_deref()
        .is_some_and(|mime| mime.starts_with("image/"))
}

/// One image part's content: the decoded bytes processed once through the
/// image child, becoming a text line and an image part. Anything that
/// cannot be decoded, or that the child refuses, ends the call failed,
/// with no content.
fn image_content(
    call: &Call,
    data: Option<&Value>,
    cancel: &dyn Cancel,
) -> Result<Vec<ContentPart>, McpError> {
    let Some(encoded) = data else {
        return Err(McpError::ImageNoData);
    };
    let Some(text) = encoded.as_str() else {
        return Err(McpError::ImageNotBase64String);
    };
    let bytes = match STANDARD.decode(text) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(McpError::ImageUndecodable(error));
        }
    };
    match call.images.process(&bytes, cancel) {
        Ok(reference) => Ok(vec![
            ContentPart::Text {
                text: format!(
                    "Image: {}x{} {}.\n",
                    reference.width, reference.height, reference.mime_type,
                ),
            },
            ContentPart::Image {
                path: reference.path,
                mime_type: reference.mime_type,
                width: reference.width,
                height: reference.height,
            },
        ]),
        Err(ImageError::Unreadable(message)) => Err(McpError::ImageUnreadable { message }),
        Err(ImageError::Failed(message)) => Err(McpError::ImageFailed { message }),
        Err(ImageError::Cancelled) => Err(McpError::CallCancelled {
            server: call.server.clone(),
            tool: call.tool.clone(),
        }),
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
    let hex = support::sha256_hex(bytes);
    format!("mcp-{}.bin", &hex[..16])
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
