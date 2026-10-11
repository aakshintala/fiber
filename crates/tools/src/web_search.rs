//! The `web_search` declarations (`docs/tools.md`, "web_search"): the
//! hosted one, which the provider runs before Fiber sees the call, and
//! Fiber's own, which runs an installed search backend.

use std::sync::Arc;

use contract::ErrorCode;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::search::{Domains, SearchBackend, SearchResult};
use contract::shapes::Effect;
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::tool_util::{failed, text_output};

/// A search the model's provider hosts. It is declared as the vendor's tool
/// type and name only, and carries the effects and the guideline line a
/// hosted call's log lines and the system prompt use. It never runs: the
/// provider ran the search, so Fiber never reviews or runs the call.
pub struct HostedSearch {
    kind: String,
}

impl HostedSearch {
    /// A hosted search of the vendor's tool type `kind`, such as
    /// `web_search_20250305`.
    pub fn new(kind: String) -> Self {
        Self { kind }
    }
}

impl Tool for HostedSearch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".to_owned(),
            description: String::new(),
            input_schema: json!({}),
            deferred: false,
            hosted: Some(self.kind.clone()),
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(crate::tool_util::effects(
            vec![Effect::Network],
            true,
            None,
            Some(String::new()),
            None,
        ))
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        failed(
            ErrorCode::ToolError,
            "The provider runs a hosted search; Fiber has none to run.".to_owned(),
        )
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("web_search")
    }
}

/// The backend call's arguments, checked against the schema before the
/// call runs.
#[derive(Debug, Deserialize)]
struct Args {
    query: String,
    allowed_domains: Option<Vec<String>>,
    blocked_domains: Option<Vec<String>>,
}

/// Fiber's own `web_search` over an installed search backend
/// (`docs/tools.md`, "Fiber's own, over a backend"): the model calls it
/// as an ordinary function tool, and Fiber runs the backend, writing its
/// results under the 16 KiB default cap.
pub struct BackendSearch {
    backend: Arc<dyn SearchBackend>,
}

impl BackendSearch {
    /// `web_search` over `backend`.
    pub fn new(backend: Arc<dyn SearchBackend>) -> Self {
        Self { backend }
    }
}

impl Tool for BackendSearch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".to_owned(),
            description: "Searches the web over the installed search backend. \
                 Takes the query and, optionally, either `allowed_domains` or \
                 `blocked_domains`, never both. Each result gives its title, URL and snippet."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "minLength": 1,
                        "description": "What to search for."
                    },
                    "allowed_domains": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Search only these domains."
                    },
                    "blocked_domains": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Search every domain but these."
                    }
                },
                "required": ["query"]
            }),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(crate::tool_util::effects(
            vec![Effect::Network],
            true,
            None,
            Some(String::new()),
            None,
        ))
    }

    /// Runs the backend on the query and the domain filter. Both domain
    /// filters present fails `invalid_arguments` before the backend is
    /// called. A backend past its timeout fails `timeout`; any other
    /// backend failure fails `tool_error` with its message. A call `cancel`
    /// stopped returns no content and no error, so the loop completes it
    /// `cancelled` (`docs/tools.md`, "Cancellation").
    fn run(&self, arguments: &Map<String, Value>, cancel: &dyn Cancel, _emit: &dyn Emit) -> Output {
        let args: Args = match crate::tool_util::arguments(arguments) {
            Ok(args) => args,
            Err(message) => return failed(ErrorCode::InvalidArguments, message),
        };
        if args.query.is_empty() {
            return failed(
                ErrorCode::InvalidArguments,
                "`query` must be a non-empty string.".to_owned(),
            );
        }
        if args.allowed_domains.is_some() && args.blocked_domains.is_some() {
            return failed(
                ErrorCode::InvalidArguments,
                "Pass either `allowed_domains` or `blocked_domains`, never both.".to_owned(),
            );
        }
        let domains = match (args.allowed_domains, args.blocked_domains) {
            (Some(allowed), None) => Domains::Allowed(allowed),
            (None, Some(blocked)) => Domains::Blocked(blocked),
            (None, None) => Domains::Any,
            (Some(_), Some(_)) => unreachable!("both filters were rejected above"),
        };
        match self.backend.search(&args.query, &domains, cancel) {
            Ok(None) => Output::default(),
            Ok(Some(results)) => text_output(render(&results)),
            Err(failure) => failed(failure.code, failure.message),
        }
    }

    fn guidelines(&self) -> Option<String> {
        crate::guidelines::of("web_search")
    }
}

/// The backend's results in order, each as its title, URL and snippet on
/// three lines, with a blank line between results. An empty list is a
/// success with nothing to show.
fn render(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return "No results.".to_owned();
    }
    results
        .iter()
        .enumerate()
        .map(|(at, result)| {
            format!(
                "{}. {}\n{}\n{}",
                at + 1,
                result.title,
                result.url,
                result.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
#[path = "web_search_tests.rs"]
mod tests;
