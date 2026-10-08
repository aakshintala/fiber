//! The hosted `web_search` declaration (`docs/tools.md`, "web_search" and
//! "Hosted by the provider"): the vendor's own search, which the provider
//! runs before Fiber sees the call.

use contract::ErrorCode;
use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::{Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use crate::files::failed;

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
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Network],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
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

#[cfg(test)]
#[path = "web_search_tests.rs"]
mod tests;
