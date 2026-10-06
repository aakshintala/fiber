//! Each tool in Chat Completions' shape, in name order.

use contract::provider::ToolDefinition;
use serde_json::{Map, Value, json};

use crate::anthropic_messages::{MAX_STRICT_TOOLS, complex_enum};
use crate::{Endpoint, strict};

/// `strict` per tool (`docs/model-routing.md`, "Protocols and
/// providers"). For an Anthropic model, Anthropic's limits apply too: no
/// enum with an object or array value, and at most 20 strict tools
/// (platform.claude.com/docs/en/build-with-claude/structured-outputs,
/// "JSON Schema limitations"; `anthropic_messages`). OpenRouter forwards
/// `strict` to Anthropic when the `structured-outputs-2025-11-13` beta
/// header is sent, and strips it otherwise
/// (openrouter.ai/docs/guides/routing/provider-selection, "Anthropic beta
/// features").
/// A model without deferral declares every tool in full (docs/tools.md,
/// "Deferral is a property of the model"); Chat Completions has no
/// defer_loading. This is the tools Fiber builds.
pub(crate) fn wire_tools(endpoint: &Endpoint, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
    let mut sorted: Vec<&ToolDefinition> = tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let compat = &endpoint.compat;
    let mut strict_left = MAX_STRICT_TOOLS;
    sorted
        .into_iter()
        .map(|tool| {
            let mut strict = strict::fits(&tool.input_schema);
            if compat.anthropic {
                strict = strict && strict_left > 0 && !complex_enum(&tool.input_schema);
                strict_left -= usize::from(strict);
            }
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                    "strict": strict,
                },
            })
            .as_object()
            .cloned()
            .unwrap_or_default()
        })
        .collect()
}
