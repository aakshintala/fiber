//! The `mcp_resources` tool (`docs/mcp.md`, "Prompts and resources"): one
//! tool reaching every configured server that offers resources, with two
//! actions. `list` with no server names the servers that offer resources
//! and starts none; with a server it lists one page of that server's
//! resources, continued by passing the answer's `nextCursor` as `cursor`.
//! `read` reads one resource by its URI, whether or not `list` showed it,
//! and its content is mapped as a tool result's is ("Calls"). The
//! definition is fixed text naming no server, so it is byte-identical
//! wherever it is declared.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use contract::emit::Emit;
use contract::images::Images;
use contract::provider::ToolDefinition;
use contract::shapes::ContentPart;
use contract::tool::{Cancel, Effects, Output, Tool};
use serde_json::{Map, Value};

use crate::effects::Hints;
use crate::elicit::Asker;
use crate::error::McpError;
use crate::fail;
use crate::rpc::{RESOURCES_LIST, RESOURCES_READ};
use crate::server_json::{CallResult, Content, ListedResource, Resource, entries, from_object};
use crate::slot::{Served, Slot};
use crate::tool::{Call, Target, answer, faulted};

/// The tool's name.
pub const RESOURCES_TOOL: &str = "mcp_resources";

/// The tool's definition: the one source, fixed text naming no server.
pub fn resources_definition() -> ToolDefinition {
    ToolDefinition {
        name: RESOURCES_TOOL.to_owned(),
        description: "Lists and reads the resources of the MCP servers that offer them. `list` \
            with no `server` names those servers. `list` with a `server` lists its resources; \
            when the answer has a `nextCursor`, `list` again with it as `cursor` for the next \
            page. `read` reads the resource at `uri` from `server`, including one a resource \
            link names."
            .to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "read"], "description": "`list` or `read`."},
                "server": {"type": "string", "description": "The MCP server's configured name."},
                "uri": {"type": "string", "description": "The resource's URI, for `read`."},
                "cursor": {"type": "string", "description": "The `nextCursor` of the previous `list` answer."},
            },
            "required": ["action"],
        }),
        deferred: false,
        hosted: None,
    }
}

/// One `mcp_resources` registration, reaching its own start's slots.
pub(crate) struct Resources {
    slots: Arc<Mutex<Vec<Arc<Slot>>>>,
    artifacts: PathBuf,
    images: Arc<dyn Images>,
}

impl Resources {
    /// One tool reaching `slots`, writing binary content to `artifacts`
    /// and processing images once through `images`.
    pub(crate) fn new(
        slots: &Arc<Mutex<Vec<Arc<Slot>>>>,
        artifacts: &Path,
        images: &Arc<dyn Images>,
    ) -> Self {
        Self {
            slots: Arc::clone(slots),
            artifacts: artifacts.to_path_buf(),
            images: Arc::clone(images),
        }
    }

    /// The tool's definition.
    fn definition(&self) -> ToolDefinition {
        resources_definition()
    }

    /// A snapshot of the slots this registration reaches, including every
    /// adoption into its own start.
    fn snapshot(&self) -> Vec<Arc<Slot>> {
        support::lock(&self.slots).clone()
    }

    /// The first slot named `server` that offers resources, in list order.
    fn lookup(slots: &[Arc<Slot>], server: &str) -> Option<Arc<Slot>> {
        slots
            .iter()
            .find(|slot| slot.spec().name == server && slot.offers_resources())
            .cloned()
    }

    /// The call through `slot`: `serve`, then one `resources/list` or
    /// `resources/read` request, mapping every outcome with `serve`'s
    /// records carried. `read` maps the result as embedded resources;
    /// `list` reads one page. With an asker the call registers before
    /// its line is sent, as a tool's does.
    fn call(
        &self,
        slot: &Arc<Slot>,
        method: &str,
        params: &Value,
        read: bool,
        cancel: &dyn Cancel,
        asker: Option<&dyn contract::tool::Ask>,
    ) -> Output {
        let server = slot.spec().name.clone();
        match slot.serve() {
            Served::Failed(failed) => Output {
                error: Some(contract::shapes::Failure::from(&failed.error)),
                servers: failed.records,
                ..Output::default()
            },
            Served::Up(running, records) => {
                let target = Target {
                    timeout: slot.spec().call_timeout,
                    artifacts: self.artifacts.clone(),
                    images: Arc::clone(&self.images),
                    slot: Arc::downgrade(slot),
                };
                let called = match asker {
                    None => slot.call(&running, method, params, slot.spec().call_timeout, cancel),
                    Some(ask) => slot.call_asking(
                        &running,
                        method,
                        params,
                        slot.spec().call_timeout,
                        cancel,
                        Asker {
                            ask,
                            server: &server,
                        },
                    ),
                };
                let call = Call::new(&server, method, target);
                match called {
                    Ok(result) => {
                        if read {
                            match contents(result) {
                                Some(result) => {
                                    let mut answered = answer(&call, &result, cancel);
                                    answered.servers = records;
                                    answered
                                }
                                None => {
                                    let mut out =
                                        fail::failed(&McpError::ResourceContentsMissing {
                                            server: server.clone(),
                                        });
                                    out.servers = records;
                                    out
                                }
                            }
                        } else {
                            match page(&server, result) {
                                Ok(text) => Output {
                                    content: vec![ContentPart::Text { text }],
                                    servers: records,
                                    ..Output::default()
                                },
                                Err(error) => {
                                    let mut out = fail::failed(&error);
                                    out.servers = records;
                                    out
                                }
                            }
                        }
                    }
                    Err(fault) => faulted(&call, fault, records),
                }
            }
        }
    }
}

impl Tool for Resources {
    fn definition(&self) -> ToolDefinition {
        self.definition()
    }

    fn effects(
        &self,
        arguments: &Map<String, Value>,
    ) -> Result<Effects, contract::tool::EffectsError> {
        // `reads`, plus `network` when `server` names a remote server that
        // offers resources: the same lookup `run` uses.
        let remote = arguments
            .get("server")
            .and_then(Value::as_str)
            .is_some_and(|name| {
                self.snapshot().iter().any(|slot| {
                    slot.spec().name == name && slot.offers_resources() && slot.spec().remote()
                })
            });
        Ok(Effects {
            declared: Hints {
                read_only: Some(true),
                destructive: None,
                open_world: Some(false),
            }
            .declared_for(remote),
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
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
        self.run_with(arguments, cancel, Some(ask))
    }
}

impl Resources {
    /// Runs the call, asking elicitations through `asker` when it is
    /// `Some`, through the same slot seam a tool's calls use.
    fn run_with(
        &self,
        arguments: &Map<String, Value>,
        cancel: &dyn Cancel,
        asker: Option<&dyn contract::tool::Ask>,
    ) -> Output {
        let request = match request(arguments) {
            Ok(request) => request,
            Err(error) => return fail::failed(&error),
        };
        let slots = self.snapshot();
        match &request {
            Request::Servers => Output {
                content: vec![ContentPart::Text {
                    text: servers_text(&slots),
                }],
                ..Output::default()
            },
            Request::List { server, cursor } => {
                let Some(slot) = Self::lookup(&slots, server) else {
                    return fail::failed(&McpError::NoResources {
                        server: server.clone(),
                    });
                };
                let params = match cursor {
                    Some(cursor) => serde_json::json!({"cursor": cursor}),
                    None => serde_json::json!({}),
                };
                self.call(&slot, RESOURCES_LIST, &params, false, cancel, asker)
            }
            Request::Read { server, uri } => {
                let Some(slot) = Self::lookup(&slots, server) else {
                    return fail::failed(&McpError::NoResources {
                        server: server.clone(),
                    });
                };
                let params = serde_json::json!({"uri": uri});
                self.call(&slot, RESOURCES_READ, &params, true, cancel, asker)
            }
        }
    }
}

/// One parsed `mcp_resources` call.
#[derive(Debug, Clone, PartialEq)]
enum Request {
    /// `list` with no `server`: name the servers that offer resources.
    Servers,
    /// `list` one page of `server`'s resources, continuing after `cursor`.
    List {
        /// The server's configured name.
        server: String,
        /// The previous answer's `nextCursor`, when continuing.
        cursor: Option<String>,
    },
    /// `read` the resource at `uri` from `server`.
    Read {
        /// The server's configured name.
        server: String,
        /// The resource's URI.
        uri: String,
    },
}

/// Reads the arguments: `action` is required; `read` needs `server` and
/// `uri`; `cursor` is used only by `list` with a `server`, and anywhere
/// else `cursor` and `uri` are ignored. A missing or non-string required
/// field is its own invalid-arguments case.
fn request(arguments: &Map<String, Value>) -> Result<Request, McpError> {
    let action = arguments.get("action").and_then(Value::as_str);
    match action {
        Some("list") => {
            let server = match arguments.get("server") {
                None => return Ok(Request::Servers),
                Some(value) => value
                    .as_str()
                    .ok_or(McpError::ResourceFieldNotString { field: "server" })?
                    .to_owned(),
            };
            let cursor = match arguments.get("cursor") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or(McpError::ResourceFieldNotString { field: "cursor" })?
                        .to_owned(),
                ),
            };
            Ok(Request::List { server, cursor })
        }
        Some("read") => {
            let server = match arguments.get("server") {
                None => return Err(McpError::ResourceReadNeedsServerAndUri),
                Some(value) => value
                    .as_str()
                    .ok_or(McpError::ResourceFieldNotString { field: "server" })?
                    .to_owned(),
            };
            let uri = match arguments.get("uri") {
                None => return Err(McpError::ResourceReadNeedsServerAndUri),
                Some(value) => value
                    .as_str()
                    .ok_or(McpError::ResourceFieldNotString { field: "uri" })?
                    .to_owned(),
            };
            Ok(Request::Read { server, uri })
        }
        _ => Err(McpError::ResourceActionUnknown),
    }
}

/// Reads one `resources/list` page: compact JSON holding the entries that
/// kept a string `uri`, with the answer's string `nextCursor` passed on.
/// Anything but an object with a `resources` array is the message the
/// call fails with.
fn page(server: &str, value: Value) -> Result<String, McpError> {
    let missing = || McpError::ResourceListMissing {
        server: server.to_owned(),
    };
    let object = value.as_object().ok_or_else(missing)?;
    let listed = object
        .get("resources")
        .and_then(Value::as_array)
        .ok_or_else(missing)?;
    let kept: Vec<ListedResource> = entries(listed.clone());
    let resources = serde_json::to_string(&kept).unwrap_or_default();
    match object.get("nextCursor").and_then(Value::as_str) {
        Some(cursor) => {
            let cursor = serde_json::to_string(cursor).unwrap_or_default();
            Ok(format!(
                "{{\"resources\":{resources},\"nextCursor\":{cursor}}}"
            ))
        }
        None => Ok(format!("{{\"resources\":{resources}}}")),
    }
}

/// Reads one `resources/read` result as embedded resources for
/// [`answer`]: each entry becomes one `Content::Resource`, and a
/// non-object entry reads as the default, giving "An unreadable
/// resource.". Anything but an object with a `contents` array is `None`,
/// and the call fails.
fn contents(value: Value) -> Option<CallResult> {
    let listed = value.as_object()?.get("contents")?.as_array()?.clone();
    let mut content = Vec::new();
    for entry in listed {
        let resource: Resource = from_object(entry).unwrap_or_default();
        content.push(Content::Resource { resource });
    }
    Some(CallResult {
        content,
        is_error: false,
    })
}

/// The servers that offer resources, sorted by name: `list` with no
/// server names them and starts none.
fn servers_text(slots: &[Arc<Slot>]) -> String {
    let mut names: Vec<String> = slots
        .iter()
        .filter(|slot| slot.offers_resources())
        .map(|slot| slot.spec().name)
        .collect();
    names.sort();
    let servers = serde_json::to_string(&names).unwrap_or_default();
    format!("{{\"servers\":{servers}}}")
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;
