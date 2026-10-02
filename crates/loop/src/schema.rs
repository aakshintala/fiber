//! Tool arguments against their input schema (`docs/tools.md`, "Before a call
//! runs"): the repair made where the schema allows exactly one reading, and
//! the check. The schema is the subset `docs/dependencies.md`, "Written
//! ourselves", names; any other keyword is skipped.

use contract::events::{ArgumentRepair, Repair, RepairFix};
use serde_json::{Map, Number, Value};

/// The repair `schema` allows to `arguments`: the one distinct reading of
/// them that passes the whole schema, or `None` when there is not exactly one
/// or it needed no repair.
pub(crate) fn repair(schema: &Value, arguments: &Value) -> Option<ArgumentRepair> {
    let mut passing = readings(schema, arguments, "")
        .into_iter()
        .filter(|(reading, _)| check(schema, reading).is_empty());
    let (Value::Object(repaired), repairs) = passing.next()? else {
        return None;
    };
    let one = passing.next().is_none() && !repairs.is_empty();
    one.then_some(ArgumentRepair { repaired, repairs })
}

/// Every way `value` fails `schema`, one line per bad field.
pub(crate) fn check(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    walk(schema, value, "", &mut errors);
    errors
}

/// A reading of a value, with the repairs that make it from the value sent.
type Reading = (Value, Vec<Repair>);

/// Every distinct reading of `value` under `schema`, from every `anyOf`
/// branch, nested ones included, whether or not it passes.
fn readings(schema: &Value, value: &Value, path: &str) -> Vec<Reading> {
    let mut all: Vec<Reading> = Vec::new();
    for (read, made) in plain(schema, value, path) {
        let Some(Value::Array(options)) = schema.get("anyOf") else {
            add(&mut all, read, made);
            continue;
        };
        for option in options {
            for (inner, more) in readings(option, &read, path) {
                add(&mut all, inner, made.iter().cloned().chain(more).collect());
            }
        }
    }
    all
}

/// Adds `value` to `all` unless an equal reading is already there.
fn add(all: &mut Vec<Reading>, value: Value, made: Vec<Repair>) {
    if !all.iter().any(|(seen, _)| *seen == value) {
        all.push((value, made));
    }
}

/// The readings of `value` under every keyword of `schema` but `anyOf`.
fn plain(schema: &Value, value: &Value, path: &str) -> Vec<Reading> {
    let allowed = types(schema);
    let wants = |name: &str| allowed.contains(&name);
    match value {
        Value::String(text) if !wants("string") => match parse(schema, text, &wants) {
            Some((parsed, fix)) => vec![(
                parsed,
                vec![Repair {
                    path: path.to_owned(),
                    fix,
                }],
            )],
            None => vec![(value.clone(), Vec::new())],
        },
        Value::Object(map) => {
            let required = required(schema);
            let mut all = vec![(value.clone(), Vec::new())];
            for (key, property) in properties(schema) {
                let at = format!("{path}/{}", escape(key));
                let drop = map.get(key).is_some_and(Value::is_null)
                    && !required.contains(&key.as_str())
                    && !check(property, &Value::Null).is_empty();
                if drop {
                    for (reading, made) in &mut all {
                        if let Value::Object(reading) = reading {
                            reading.remove(key);
                        }
                        made.push(Repair {
                            path: at.clone(),
                            fix: RepairFix::NullDropped,
                        });
                    }
                } else if let Some(inner) = map.get(key) {
                    all = product(all, &readings(property, inner, &at), key.as_str());
                }
            }
            all
        }
        Value::Array(items) => {
            let mut all = vec![(value.clone(), Vec::new())];
            if let Some(each) = schema.get("items") {
                for (i, item) in items.iter().enumerate() {
                    all = product(all, &readings(each, item, &format!("{path}/{i}")), i);
                }
            }
            all
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            vec![(value.clone(), Vec::new())]
        }
    }
}

/// Every reading in `all` with its part at `at` replaced by each of `inner`.
fn product<I: serde_json::value::Index + Copy>(
    all: Vec<Reading>,
    inner: &[Reading],
    at: I,
) -> Vec<Reading> {
    let mut out = Vec::with_capacity(all.len() * inner.len());
    for (reading, made) in &all {
        for (part, more) in inner {
            let mut reading = reading.clone();
            if let Some(slot) = reading.get_mut(at) {
                *slot = part.clone();
            }
            out.push((reading, made.iter().chain(more).cloned().collect()));
        }
    }
    out
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
        // Whether it then passes is the whole schema's to say (`repair`).
        if matches!(&parsed, Value::Object(_) | Value::Array(_)) {
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
