//! Whether a tool's schema already fits OpenAI's strict subset, so `strict`
//! can be `true` (`docs/model-routing.md`, "Protocols and providers"). Fiber
//! never rewrites a schema to fit.
//!
//! The rule is conservative, from the one probed example
//! (`research/openai-responses-probe`): strict mode refused an object schema
//! without `additionalProperties: false`, and its rewrite made every
//! property required. So a schema fits only when its root is an object and:
//!
//! - every object lists `properties`, `required` names exactly those
//!   properties, and `additionalProperties` is `false`
//! - every array has `items`, which fits in turn
//! - `type` is one string, never a list
//! - no keyword appears beyond `type`, `properties`, `required`,
//!   `additionalProperties`, `items`, `enum` and `description`
//!
//! A schema outside the rule may still be one OpenAI accepts in strict mode;
//! it is sent with `strict: false`, which OpenAI accepts for any schema.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

/// The keywords a schema in the subset may use.
const KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "description",
];

/// Whether `schema` fits the strict subset.
pub(crate) fn fits(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object") && fits_node(schema)
}

fn fits_node(schema: &Value) -> bool {
    let Some(map) = schema.as_object() else {
        return false;
    };
    if !map.keys().all(|key| KEYWORDS.contains(&key.as_str())) {
        return false;
    }
    match map.get("type").and_then(Value::as_str) {
        Some("object") => fits_object(map),
        Some("array") => map.get("items").is_some_and(fits_node),
        Some("string" | "number" | "integer" | "boolean" | "null") => {
            !map.contains_key("properties") && !map.contains_key("items")
        }
        _ => false,
    }
}

fn fits_object(map: &Map<String, Value>) -> bool {
    let Some(properties) = map.get("properties").and_then(Value::as_object) else {
        return false;
    };
    let Some(required) = map.get("required").and_then(Value::as_array) else {
        return false;
    };
    let names: BTreeSet<&str> = required.iter().filter_map(Value::as_str).collect();
    let all_required = names.len() == required.len()
        && names.len() == properties.len()
        && names.iter().all(|name| properties.contains_key(*name));
    all_required
        && map.get("additionalProperties") == Some(&Value::Bool(false))
        && properties.values().all(fits_node)
}

#[cfg(test)]
#[path = "strict_tests.rs"]
mod tests;
