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
    let sorted = crate::wire::sorted_by_name(tools);
    let compat = &endpoint.compat;
    let mut strict_left = MAX_STRICT_TOOLS;
    sorted
        .into_iter()
        .map(|tool| {
            // A hosted search is the vendor's own tool alone, with its
            // fixed settings and no `function` wrapper (`docs/tools.md`,
            // "Hosted by the provider").
            if tool.hosted.as_deref() == Some("openrouter:web_search") {
                return crate::wire::object(json!({
                    "type": "openrouter:web_search",
                    "parameters": {"max_results": 5, "max_uses": 5},
                }));
            }
            let mut strict = strict::fits(&tool.input_schema);
            if compat.anthropic {
                strict = strict && strict_left > 0 && !complex_enum(&tool.input_schema);
                strict_left -= usize::from(strict);
            }
            // A Gemini gateway rejects nullable `type` arrays, so each
            // `[T, "null"]` pair is sent as `T` with `nullable: true`.
            // `strict` stays computed on the schema as declared.
            let mut parameters = tool.input_schema.clone();
            if compat.gemini {
                rewrite_nullable_types(&mut parameters);
            }
            crate::wire::object(json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": parameters,
                    "strict": strict,
                },
            }))
        })
        .collect()
}

/// Rewrites every nullable `type` pair in `schema`, at every depth: an
/// object whose `type` is exactly `[T, "null"]`, in either order with `T`
/// not `"null"`, carries `type: T` with `nullable: true`, overwriting any
/// existing `nullable` value. Everything else passes through untouched, and
/// there is no `$ref` to resolve: tool schemas are inline values. An
/// `enum`, `const`, `default` or `examples` value is data, not a schema,
/// so the walk never descends into one: an object inside an `enum` is a
/// value the model may send back, not a shape to rewrite.
fn rewrite_nullable_types(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            if let Some(single) = nullable_single(map) {
                map.insert("type".to_owned(), Value::String(single));
                // A required `nullable: true` wins over any existing value.
                map.insert("nullable".to_owned(), Value::Bool(true));
            }
            for (key, value) in map {
                if !matches!(key.as_str(), "enum" | "const" | "default" | "examples") {
                    rewrite_nullable_types(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_nullable_types(item);
            }
        }
        _ => {}
    }
}

/// The single non-null member of a nullable `type` pair: a two-element
/// array holding `"null"` and one other string, which must not itself be
/// `"null"`.
fn nullable_single(map: &Map<String, Value>) -> Option<String> {
    let types = map.get("type")?.as_array()?;
    if types.len() != 2 {
        return None;
    }
    let mut names = types.iter().filter_map(Value::as_str);
    let (first, second) = (names.next()?, names.next()?);
    match (first, second) {
        ("null", other) | (other, "null") if other != "null" => Some(other.to_owned()),
        _ => None,
    }
}
