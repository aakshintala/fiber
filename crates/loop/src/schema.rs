//! Tool arguments against their input schema (`docs/tools.md`, "Before a call
//! runs"): the repair made where the schema allows exactly one reading, and
//! the check. The schema is the subset `docs/dependencies.md`, "Written
//! ourselves", names; any other keyword is skipped.

use contract::events::{ArgumentRepair, Repair, RepairFix};
use serde_json::{Map, Number, Value};

/// The repair `schema` allows to `arguments`, or `None` when nothing needed
/// one.
pub(crate) fn repair(schema: &Value, arguments: &Value) -> Option<ArgumentRepair> {
    let mut repaired = arguments.clone();
    let mut repairs = Vec::new();
    // An ambiguous `anyOf` is left as sent; the check then reports it.
    fix(schema, &mut repaired, "", &mut repairs);
    let Value::Object(repaired) = repaired else {
        return None;
    };
    (!repairs.is_empty()).then_some(ArgumentRepair { repaired, repairs })
}

/// Every way `value` fails `schema`, one line per bad field.
pub(crate) fn check(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    walk(schema, value, "", &mut errors);
    errors
}

/// Repairs `value` where `schema` allows one reading, adding each fix to
/// `repairs`. Returns whether it met an `anyOf` with more than one reading,
/// which it leaves as it was.
fn fix(schema: &Value, value: &mut Value, path: &str, repairs: &mut Vec<Repair>) -> bool {
    let mut ambiguous = plain(schema, value, path, repairs);
    if let Some(Value::Array(options)) = schema.get("anyOf")
        && !check(schema, value).is_empty()
    {
        ambiguous |= any_of(schema, options, value, path, repairs);
    }
    ambiguous
}

/// Repairs `value` by reading each of `options`, the `anyOf` of `schema`.
/// A branch's repair is a reading, kept only when it is the one distinct
/// reading that passes the whole schema and no branch met an ambiguity of
/// its own. Returns whether there was more than one reading.
fn any_of(
    schema: &Value,
    options: &[Value],
    value: &mut Value,
    path: &str,
    repairs: &mut Vec<Repair>,
) -> bool {
    let mut readings: Vec<(Value, Vec<Repair>)> = Vec::new();
    for option in options {
        let mut reading = value.clone();
        let mut made = Vec::new();
        if fix(option, &mut reading, path, &mut made) {
            return true;
        }
        let new = !readings.iter().any(|(seen, _)| *seen == reading);
        if !made.is_empty() && new && check(schema, &reading).is_empty() {
            readings.push((reading, made));
        }
    }
    match readings.as_mut_slice() {
        [] => false,
        [(reading, made)] => {
            *value = std::mem::take(reading);
            repairs.append(made);
            false
        }
        _ => true,
    }
}

/// Repairs `value` against every keyword of `schema` but `anyOf`. Returns
/// whether a property or item met an ambiguous `anyOf`.
fn plain(schema: &Value, value: &mut Value, path: &str, repairs: &mut Vec<Repair>) -> bool {
    let mut ambiguous = false;
    let allowed = types(schema);
    let wants = |name: &str| allowed.contains(&name);
    let found = match value {
        Value::String(text) if !wants("string") => parse(schema, text, &wants),
        Value::Object(map) => {
            let required = required(schema);
            for (key, property) in properties(schema) {
                let at = format!("{path}/{}", escape(key));
                let drop = map.get(key).is_some_and(Value::is_null)
                    && !required.contains(&key.as_str())
                    && !check(property, &Value::Null).is_empty();
                if drop {
                    map.remove(key);
                    repairs.push(Repair {
                        path: at,
                        fix: RepairFix::NullDropped,
                    });
                } else if let Some(inner) = map.get_mut(key) {
                    ambiguous |= fix(property, inner, &at, repairs);
                }
            }
            None
        }
        Value::Array(items) => {
            if let Some(each) = schema.get("items") {
                for (i, item) in items.iter_mut().enumerate() {
                    ambiguous |= fix(each, item, &format!("{path}/{i}"), repairs);
                }
            }
            None
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    };
    if let Some((parsed, how)) = found {
        *value = parsed;
        repairs.push(Repair {
            path: path.to_owned(),
            fix: how,
        });
    }
    ambiguous
}

/// The one value `text` reads as where `schema` does not take a string.
fn parse(schema: &Value, text: &str, wants: &dyn Fn(&str) -> bool) -> Option<(Value, RepairFix)> {
    let trimmed = text.trim();
    if wants("integer") || wants("number") {
        let number = trimmed
            .parse::<i64>()
            .map(Number::from)
            .or_else(|_| trimmed.parse::<u64>().map(Number::from))
            .ok()
            .or_else(|| {
                trimmed
                    .parse::<f64>()
                    .ok()
                    .filter(|_| wants("number"))
                    .and_then(Number::from_f64)
            });
        if let Some(number) = number {
            return Some((Value::Number(number), RepairFix::StringToNumber));
        }
    }
    if wants("boolean") {
        match trimmed {
            "true" => return Some((Value::Bool(true), RepairFix::StringToBoolean)),
            "false" => return Some((Value::Bool(false), RepairFix::StringToBoolean)),
            _ => {}
        }
    }
    if wants("object") || wants("array") {
        let parsed: Value = serde_json::from_str(trimmed).ok()?;
        let fits = matches!(&parsed, Value::Object(_) | Value::Array(_));
        if fits && check(schema, &parsed).is_empty() {
            return Some((parsed, RepairFix::StringParsed));
        }
    }
    None
}

fn walk(schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    let at = || {
        if path.is_empty() {
            "arguments".to_owned()
        } else {
            format!("`{path}`")
        }
    };
    if let Some(Value::Array(options)) = schema.get("anyOf")
        && !options.iter().any(|o| check(o, value).is_empty())
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
        Value::Object(map) => object(schema, map, path, errors),
        Value::Array(items) => {
            if let Some(each) = schema.get("items") {
                for (i, item) in items.iter().enumerate() {
                    walk(each, item, &format!("{path}/{i}"), errors);
                }
            }
        }
        Value::Null | Value::Bool(_) => {}
    }
}

fn object(schema: &Value, map: &Map<String, Value>, path: &str, errors: &mut Vec<String>) {
    for key in required(schema) {
        if !map.contains_key(key) {
            errors.push(format!("`{path}/{}`: missing", escape(key)));
        }
    }
    for (key, value) in map {
        let at = format!("{path}/{}", escape(key));
        match (property(schema, key), schema.get("additionalProperties")) {
            (Some(property), _) => walk(property, value, &at, errors),
            (None, Some(Value::Bool(false))) => errors.push(format!("`{at}`: not allowed")),
            (None, Some(extra @ Value::Object(_))) => walk(extra, value, &at, errors),
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
