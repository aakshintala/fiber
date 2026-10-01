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
//! - it is within OpenAI's documented size limits
//!   (developers.openai.com/api/docs/guides/structured-outputs, "Supported
//!   schemas"): at most 5,000 object properties, 10 levels of nesting
//!   (counted here over every object and array, the root being level 1),
//!   1,000 enum values, 120,000 characters across property names and enum
//!   values, and 15,000 characters in one enum of more than 250 values. A
//!   non-string enum value counts as its JSON text.
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

/// OpenAI's documented limits on a strict schema.
const MAX_PROPERTIES: usize = 5_000;
const MAX_DEPTH: usize = 10;
const MAX_ENUM_VALUES: usize = 1_000;
const MAX_CHARS: usize = 120_000;
const LARGE_ENUM: usize = 250;
const MAX_LARGE_ENUM_CHARS: usize = 15_000;

/// What the schema has used of the limits so far.
#[derive(Default)]
struct Tally {
    properties: usize,
    enum_values: usize,
    chars: usize,
}

/// Whether `schema` fits the strict subset.
pub(crate) fn fits(schema: &Value) -> bool {
    let mut tally = Tally::default();
    schema.get("type").and_then(Value::as_str) == Some("object")
        && fits_node(schema, 1, &mut tally)
        && tally.properties <= MAX_PROPERTIES
        && tally.enum_values <= MAX_ENUM_VALUES
        && tally.chars <= MAX_CHARS
}

fn fits_node(schema: &Value, depth: usize, tally: &mut Tally) -> bool {
    let Some(map) = schema.as_object() else {
        return false;
    };
    if depth > MAX_DEPTH || !map.keys().all(|key| KEYWORDS.contains(&key.as_str())) {
        return false;
    }
    if let Some(values) = map.get("enum") {
        let Some(values) = values.as_array() else {
            return false;
        };
        let chars: usize = values
            .iter()
            .map(|v| {
                v.as_str()
                    .map_or_else(|| v.to_string().len(), |s| s.chars().count())
            })
            .sum();
        if values.len() > LARGE_ENUM && chars > MAX_LARGE_ENUM_CHARS {
            return false;
        }
        tally.enum_values += values.len();
        tally.chars += chars;
    }
    match map.get("type").and_then(Value::as_str) {
        Some("object") => fits_object(map, depth, tally),
        Some("array") => map
            .get("items")
            .is_some_and(|items| fits_node(items, depth + 1, tally)),
        Some("string" | "number" | "integer" | "boolean" | "null") => {
            !map.contains_key("properties") && !map.contains_key("items")
        }
        _ => false,
    }
}

fn fits_object(map: &Map<String, Value>, depth: usize, tally: &mut Tally) -> bool {
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
    tally.properties += properties.len();
    tally.chars += properties.keys().map(|k| k.chars().count()).sum::<usize>();
    all_required
        && map.get("additionalProperties") == Some(&Value::Bool(false))
        && properties
            .values()
            .all(|property| fits_node(property, depth + 1, tally))
}

#[cfg(test)]
#[path = "strict_tests.rs"]
mod tests;
