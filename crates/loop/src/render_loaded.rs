//! Renders logged `tool_search` results against the preamble the request
//! sends (`docs/tools.md`, "Tool search"): one output per input, in order.
//! Only `ToolSearchResult` maps; every other input crosses unchanged. On a
//! model with deferral the request sends D, so each logged list is filtered
//! to D and an empty remainder sends the zero-match text as a plain error
//! result. Without deferral every logged search renders as text.

use std::collections::BTreeSet;

use contract::provider::{Input, NO_MATCH, loaded_text};

/// Maps `conversation` for a request sending `deferred`: `Some(D)` filters
/// each `ToolSearchResult` to D in logged order, `None` renders each as
/// text. An empty result is a plain `ToolResult` carrying the zero-match
/// text, never an error.
pub(crate) fn render(conversation: Vec<Input>, deferred: Option<&BTreeSet<String>>) -> Vec<Input> {
    conversation
        .into_iter()
        .map(|input| match input {
            Input::ToolSearchResult { action_id, loaded } => match deferred {
                Some(deferred) => {
                    let kept: Vec<String> = loaded
                        .into_iter()
                        .filter(|name| deferred.contains(name))
                        .collect();
                    if kept.is_empty() {
                        plain(&action_id)
                    } else {
                        Input::ToolSearchResult {
                            action_id,
                            loaded: kept,
                        }
                    }
                }
                None => {
                    if loaded.is_empty() {
                        plain(&action_id)
                    } else {
                        Input::ToolResult {
                            action_id,
                            text: loaded_text(&loaded),
                            is_error: false,
                            images: Vec::new(),
                            pdfs: Vec::new(),
                        }
                    }
                }
            },
            other @ (Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }) => other,
        })
        .collect()
}

/// The zero-match result for `action`: the plain text result the model is
/// sent when nothing is left (`docs/tools.md`, "Tool search").
fn plain(action: &contract::ActionId) -> Input {
    Input::ToolResult {
        action_id: action.clone(),
        text: NO_MATCH.to_owned(),
        is_error: false,
        images: Vec::new(),
        pdfs: Vec::new(),
    }
}

#[cfg(test)]
#[path = "render_loaded_tests.rs"]
mod tests;
