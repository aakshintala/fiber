//! Tool arguments against their input schema (`docs/tools.md`, "Before a call
//! runs"): the repairs made where a property's schema names a single type,
//! and the check. The check covers the subset `docs/dependencies.md`,
//! "Written ourselves", names; any other keyword is skipped.

use contract::events::{ArgumentRepair, Repair, RepairFix};
use serde_json::{Map, Number, Value};

/// The repairs `schema` allows to `arguments`, or `None` when none applied.
/// One pass over the arguments.
pub(crate) fn repair(schema: &Value, arguments: &Value) -> Option<ArgumentRepair> {
    let mut repaired = arguments.clone();
    let mut repairs = Vec::new();
    fix(schema, schema, &mut repaired, "", &mut repairs);
    let Value::Object(repaired) = repaired else {
        return None;
    };
    (!repairs.is_empty()).then_some(ArgumentRepair { repaired, repairs })
}

/// Every way `value` fails `schema`, one line per bad field.
pub(crate) fn check(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    walk(schema, schema, value, "", &mut errors);
    errors
}

/// Whether `value` passes `schema`, part of `root`.
fn passes(root: &Value, schema: &Value, value: &Value) -> bool {
    let mut errors = Vec::new();
    walk(root, schema, value, "", &mut errors);
    errors.is_empty()
}

/// Repairs `value` against `schema`, part of `root`, adding each fix to
/// `repairs`. Nothing under an `anyOf` or `oneOf` is repaired.
fn fix(root: &Value, schema: &Value, value: &mut Value, path: &str, repairs: &mut Vec<Repair>) {
    let Ok(schema) = resolve(root, schema) else {
        return;
    };
    if choice(schema) {
        return;
    }
    match value {
        Value::Object(map) => {
            let required = required(schema);
            for (key, property) in properties(schema) {
                let at = format!("{path}/{}", escape(key));
                let Some(inner) = map.get_mut(key) else {
                    continue;
                };
                let drop = inner.is_null()
                    && !required.contains(&key.as_str())
                    && resolve(root, property)
                        .ok()
                        .and_then(single)
                        .is_some_and(|kind| kind != "null");
                if drop {
                    map.remove(key);
                    repairs.push(Repair {
                        path: at,
                        fix: RepairFix::NullDropped,
                    });
                } else {
                    fix(root, property, inner, &at, repairs);
                }
            }
        }
        Value::Array(items) => {
            if let Some(each) = schema.get("items") {
                for (i, item) in items.iter_mut().enumerate() {
                    fix(root, each, item, &format!("{path}/{i}"), repairs);
                }
            }
        }
        Value::String(text) => {
            let Some(kind) = single(schema) else {
                return;
            };
            let read = match kind {
                "integer" | "number" => number(text, kind == "integer")
                    .map(|n| (Value::Number(n), RepairFix::StringToNumber)),
                "boolean" => match text.as_str() {
                    "true" => Some((Value::Bool(true), RepairFix::StringToBoolean)),
                    "false" => Some((Value::Bool(false), RepairFix::StringToBoolean)),
                    _ => None,
                },
                "array" | "object" => parse(root, schema, kind, text, path, repairs),
                _ => None,
            };
            if let Some((read, how)) = read {
                *value = read;
                repairs.push(Repair {
                    path: path.to_owned(),
                    fix: how,
                });
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// `text` parsed as the `kind` (array or object) `schema` names, repaired
/// inside, where the result passes `schema`. The repairs inside go to
/// `repairs` only then.
fn parse(
    root: &Value,
    schema: &Value,
    kind: &str,
    text: &str,
    path: &str,
    repairs: &mut Vec<Repair>,
) -> Option<(Value, RepairFix)> {
    let mut parsed: Value = serde_json::from_str(text).ok()?;
    if !is(&parsed, kind) {
        return None;
    }
    let mut inside = Vec::new();
    fix(root, schema, &mut parsed, path, &mut inside);
    if !passes(root, schema, &parsed) {
        return None;
    }
    repairs.append(&mut inside);
    Some((parsed, RepairFix::StringParsed))
}

/// `text` as a plain JSON number, whole where `integer` is asked for.
fn number(text: &str, integer: bool) -> Option<Number> {
    // JSON allows whitespace around a number; a repair does not.
    if text.trim() != text {
        return None;
    }
    let number: Number = serde_json::from_str(text).ok()?;
    if !integer || number.is_i64() || number.is_u64() {
        return Some(number);
    }
    let whole = number.as_f64().filter(|f| f.fract() == 0.0)?;
    format!("{whole:.0}").parse::<i64>().ok().map(Number::from)
}

/// The one type `schema` names, where it names one and has no `anyOf` or
/// `oneOf`.
fn single(schema: &Value) -> Option<&str> {
    match types(schema).as_slice() {
        [one] if !choice(schema) => Some(one),
        _ => None,
    }
}

/// Whether `schema` offers a choice of shapes, under which nothing is
/// repaired.
fn choice(schema: &Value) -> bool {
    schema.get("anyOf").is_some() || schema.get("oneOf").is_some()
}

/// `schema`, part of `root`, with each `$ref` followed. Only a `$ref` within
/// `root` is followed; the error says why one was not.
fn resolve<'a>(root: &'a Value, mut schema: &'a Value) -> Result<&'a Value, String> {
    let mut seen = Vec::new();
    while let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if seen.contains(&reference) {
            return Err(format!(
                "its schema's `$ref` to `{reference}` leads back to itself"
            ));
        }
        seen.push(reference);
        schema = reference
            .strip_prefix('#')
            .and_then(|pointer| root.pointer(pointer))
            .ok_or_else(|| format!("its schema's `$ref` to `{reference}` cannot be followed"))?;
    }
    Ok(schema)
}

fn walk(root: &Value, schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    let at = || {
        if path.is_empty() {
            "arguments".to_owned()
        } else {
            format!("`{path}`")
        }
    };
    let schema = match resolve(root, schema) {
        Ok(schema) => schema,
        Err(why) => return errors.push(format!("{}: {why}", at())),
    };
    if let Some(Value::Array(options)) = schema.get("anyOf")
        && !options.iter().any(|o| passes(root, o, value))
    {
        errors.push(format!("{}: matches none of the shapes allowed", at()));
    }
    let types = types(schema);
    if !types.is_empty() && !types.iter().any(|t| is(value, t)) {
        errors.push(format!(
            "{}: expected {}, got {}",
            at(),
            types.join(" or "),
            name(value)
        ));
        return;
    }
    if let Some(Value::Array(allowed)) = schema.get("enum")
        && !allowed.contains(value)
    {
        let allowed: Vec<String> = allowed.iter().map(Value::to_string).collect();
        errors.push(format!("{}: must be one of {}", at(), allowed.join(", ")));
    }
    match value {
        Value::Number(n) => {
            let n = n.as_f64().unwrap_or(f64::NAN);
            if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
                && n < min
            {
                errors.push(format!("{}: must be at least {min}", at()));
            }
            if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
                && n > max
            {
                errors.push(format!("{}: must be at most {max}", at()));
            }
        }
        Value::String(text) => {
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
                && u64::try_from(text.chars().count()).unwrap_or(u64::MAX) < min
            {
                errors.push(format!("{}: must be at least {min} characters", at()));
            }
        }
        Value::Object(map) => object(root, schema, map, path, errors),
        Value::Array(items) => {
            if let Some(each) = schema.get("items") {
                for (i, item) in items.iter().enumerate() {
                    walk(root, each, item, &format!("{path}/{i}"), errors);
                }
            }
        }
        Value::Null | Value::Bool(_) => {}
    }
}

fn object(
    root: &Value,
    schema: &Value,
    map: &Map<String, Value>,
    path: &str,
    errors: &mut Vec<String>,
) {
    for key in required(schema) {
        if !map.contains_key(key) {
            errors.push(format!("`{path}/{}`: missing", escape(key)));
        }
    }
    for (key, value) in map {
        let at = format!("{path}/{}", escape(key));
        match (property(schema, key), schema.get("additionalProperties")) {
            (Some(property), _) => walk(root, property, value, &at, errors),
            (None, Some(Value::Bool(false))) => errors.push(format!("`{at}`: not allowed")),
            (None, Some(extra @ Value::Object(_))) => walk(root, extra, value, &at, errors),
            (None, _) => {}
        }
    }
}

fn is(value: &Value, name: &str) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        // A type this subset does not know is skipped, not failed.
        _ => true,
    }
}

fn name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn required(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|keys| keys.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn properties(schema: &Value) -> impl Iterator<Item = (&String, &Value)> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
}

fn property<'a>(schema: &'a Value, key: &str) -> Option<&'a Value> {
    schema.get("properties").and_then(|p| p.get(key))
}

/// A key as a JSON Pointer token.
fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
