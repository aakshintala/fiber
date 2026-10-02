//! Tool arguments against their input schema (`docs/tools.md`, "Before a call
//! runs"): the repairs made where a property's schema names a single type,
//! and the check. The check covers the subset `docs/dependencies.md`,
//! "Written ourselves", names; any other keyword is skipped.
//!
//! One position is one value in the arguments. [`applying`] lists the schemas
//! that apply there; the check and the repair both read that list, and only
//! [`applying`] reads `$ref`.

use contract::events::{ArgumentRepair, Repair, RepairFix};
use serde_json::{Number, Value};

/// The repairs `schema` allows to `arguments`, or `None` when none applied.
/// One pass over the arguments.
pub(crate) fn repair(schema: &Value, arguments: &Value) -> Option<ArgumentRepair> {
    let mut repaired = arguments.clone();
    let mut repairs = Vec::new();
    repair_at(schema, &[schema], &mut repaired, "", &mut repairs);
    let Value::Object(repaired) = repaired else {
        return None;
    };
    (!repairs.is_empty()).then_some(ArgumentRepair { repaired, repairs })
}

/// Every way `value` fails `schema`, one line per bad field.
pub(crate) fn check(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    examine(schema, &[schema], value, "", &mut errors);
    errors
}

/// Checks `value` at `pointer`. A `$ref` failure is this position's one line.
fn examine(
    root: &Value,
    reaching: &[&Value],
    value: &Value,
    pointer: &str,
    errors: &mut Vec<String>,
) {
    if let Err(why) = check_list(root, reaching, &[], value, pointer, errors) {
        errors.push(format!("{}: {why}", place(pointer)));
    }
}

/// Checks `value` against [`applying`]'s list. `guard` is the schemas already
/// followed on this path. `Err` is a `$ref` failure, not yet placed.
fn check_list<'a>(
    root: &'a Value,
    reaching: &[&'a Value],
    guard: &[&'a Value],
    value: &Value,
    pointer: &str,
    errors: &mut Vec<String>,
) -> Result<(), String> {
    // Each schema that reaches the position starts from the same guard, so
    // two of them may `$ref` one definition without that being a cycle.
    let mut chains = Vec::with_capacity(reaching.len());
    for schema in reaching {
        chains.push(applying(root, &[*schema], guard)?);
    }
    let mut open = Vec::new();
    for chain in &chains {
        for (index, schema) in chain.iter().enumerate() {
            let here = chain_guard(guard, chain, index);
            if !any_of(root, schema, &here, value, pointer)? {
                errors.push(format!(
                    "{}: matches none of the shapes allowed",
                    place(pointer)
                ));
            }
            let kinds = types(schema);
            if !kinds.is_empty() && !kinds.iter().any(|kind| is(value, kind)) {
                errors.push(format!(
                    "{}: expected {}, got {}",
                    place(pointer),
                    kinds.join(" or "),
                    name(value)
                ));
                // A type failure skips the rest of this schema's keywords,
                // not the other schemas at this position.
                continue;
            }
            if let Some(Value::Array(allowed)) = schema.get("enum")
                && !allowed.contains(value)
            {
                let allowed: Vec<String> = allowed.iter().map(Value::to_string).collect();
                errors.push(format!(
                    "{}: must be one of {}",
                    place(pointer),
                    allowed.join(", ")
                ));
            }
            match value {
                Value::Number(n) => bounds(schema, n, pointer, errors),
                Value::String(text) => min_length(schema, text, pointer, errors),
                Value::Object(_) | Value::Array(_) | Value::Bool(_) | Value::Null => {}
            }
            open.push(*schema);
        }
    }
    match value {
        Value::Object(map) => check_object(root, &open, map, pointer, errors),
        Value::Array(items) => check_items(root, &open, items, pointer, errors),
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
    Ok(())
}

/// The schemas that apply at one position: each of `reaching`, then every
/// schema its `$ref` chain reaches. `path` is the schemas followed so far
/// along this path of `$ref` and `anyOf` steps.
fn applying<'a>(
    root: &'a Value,
    reaching: &[&'a Value],
    path: &[&'a Value],
) -> Result<Vec<&'a Value>, String> {
    let mut list = Vec::new();
    for start in reaching {
        let mut seen = path.to_vec();
        let mut current = *start;
        loop {
            list.push(current);
            let Some(reference) = current.get("$ref").and_then(Value::as_str) else {
                break;
            };
            let Some(target) = reference
                .strip_prefix('#')
                .and_then(|pointer| root.pointer(pointer))
            else {
                return Err(format!(
                    "its schema's `$ref` to `{reference}` cannot be followed"
                ));
            };
            if seen.iter().any(|schema| std::ptr::eq(*schema, target)) {
                return Err(format!(
                    "its schema's `$ref` to `{reference}` leads back to itself"
                ));
            }
            seen.push(target);
            current = target;
        }
    }
    Ok(list)
}

/// `incoming` plus this chain's `$ref` targets up through `chain[index]`.
/// The schema that reached the chain is not a target, so two chains may
/// reach one definition without that being a cycle.
fn chain_guard<'a>(incoming: &[&'a Value], chain: &[&'a Value], index: usize) -> Vec<&'a Value> {
    let mut guard = incoming.to_vec();
    if let Some(followed) = chain.get(1..=index) {
        guard.extend_from_slice(followed);
    }
    guard
}

/// Whether `value` matches one `anyOf` branch. No `anyOf` matches. A `$ref`
/// failure is `Err`: it belongs to this position, not to the branch.
fn any_of<'a>(
    root: &'a Value,
    schema: &'a Value,
    guard: &[&'a Value],
    value: &Value,
    pointer: &str,
) -> Result<bool, String> {
    let Some(Value::Array(options)) = schema.get("anyOf") else {
        return Ok(true);
    };
    for option in options {
        let mut local = Vec::new();
        check_list(root, &[option], guard, value, pointer, &mut local)?;
        if local.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn bounds(schema: &Value, n: &Number, pointer: &str, errors: &mut Vec<String>) {
    let n = n.as_f64().unwrap_or(f64::NAN);
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
        && n < min
    {
        errors.push(format!("{}: must be at least {min}", place(pointer)));
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
        && n > max
    {
        errors.push(format!("{}: must be at most {max}", place(pointer)));
    }
}

fn min_length(schema: &Value, text: &str, pointer: &str, errors: &mut Vec<String>) {
    if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
        && u64::try_from(text.chars().count()).unwrap_or(u64::MAX) < min
    {
        errors.push(format!(
            "{}: must be at least {min} characters",
            place(pointer)
        ));
    }
}

fn check_object(
    root: &Value,
    schemas: &[&Value],
    map: &serde_json::Map<String, Value>,
    pointer: &str,
    errors: &mut Vec<String>,
) {
    for schema in schemas {
        for key in required(schema) {
            if !map.contains_key(key) {
                errors.push(format!("`{pointer}/{}`: missing", escape(key)));
            }
        }
    }
    for (key, child) in map {
        let at = format!("{pointer}/{}", escape(key));
        for schema in schemas {
            if property(schema, key).is_none()
                && matches!(schema.get("additionalProperties"), Some(Value::Bool(false)))
            {
                errors.push(format!("`{at}`: not allowed"));
            }
        }
        let children = child_schemas(schemas, key);
        if !children.is_empty() {
            // A property is a new position: its guard starts empty.
            examine(root, &children, child, &at, errors);
        }
    }
}

/// The schemas that reach `key` from `schemas`: a `properties` entry, or
/// `additionalProperties` when that is a schema and the key is not named.
fn child_schemas<'a>(schemas: &[&'a Value], key: &str) -> Vec<&'a Value> {
    let mut children = Vec::new();
    for schema in schemas {
        if let Some(property) = property(schema, key) {
            children.push(property);
        } else if let Some(extra @ Value::Object(_)) = schema.get("additionalProperties") {
            children.push(extra);
        }
    }
    children
}

fn check_items(
    root: &Value,
    schemas: &[&Value],
    items: &[Value],
    pointer: &str,
    errors: &mut Vec<String>,
) {
    let item_schemas: Vec<&Value> = schemas
        .iter()
        .filter_map(|schema| schema.get("items"))
        .collect();
    if item_schemas.is_empty() {
        return;
    }
    for (i, item) in items.iter().enumerate() {
        examine(root, &item_schemas, item, &format!("{pointer}/{i}"), errors);
    }
}

/// Repairs `value` from the schemas that reach it. Nothing here or below is
/// repaired when the list has `anyOf` or `oneOf`, or cannot be built.
fn repair_at(
    root: &Value,
    reaching: &[&Value],
    value: &mut Value,
    pointer: &str,
    repairs: &mut Vec<Repair>,
) {
    let Some(list) = repaired_list(root, reaching) else {
        return;
    };
    match value {
        Value::Object(map) => repair_object(root, &list, map, pointer, repairs),
        Value::Array(items) => {
            let item_schemas: Vec<&Value> = list
                .iter()
                .filter_map(|schema| schema.get("items"))
                .collect();
            if item_schemas.is_empty() {
                return;
            }
            for (i, item) in items.iter_mut().enumerate() {
                repair_at(
                    root,
                    &item_schemas,
                    item,
                    &format!("{pointer}/{i}"),
                    repairs,
                );
            }
        }
        Value::String(text) => {
            let Some(kind) = single_type(&list) else {
                return;
            };
            let Some((read, how)) = read_string(root, reaching, kind, text, pointer, repairs)
            else {
                return;
            };
            *value = read;
            repairs.push(Repair {
                path: pointer.to_owned(),
                fix: how,
            });
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn repaired_list<'a>(root: &'a Value, reaching: &[&'a Value]) -> Option<Vec<&'a Value>> {
    let list = applying(root, reaching, &[]).ok()?;
    let blocked = list
        .iter()
        .any(|schema| schema.get("anyOf").is_some() || schema.get("oneOf").is_some());
    (!blocked).then_some(list)
}

fn repair_object(
    root: &Value,
    list: &[&Value],
    map: &mut serde_json::Map<String, Value>,
    pointer: &str,
    repairs: &mut Vec<Repair>,
) {
    let required_keys = union_required(list);
    // Owned keys, so a null can be removed from `map` while the rest are repaired.
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        let subs = child_schemas(list, &key);
        if subs.is_empty() {
            continue;
        }
        let at = format!("{pointer}/{}", escape(&key));
        let drop_null = map.get(&key).is_some_and(|inner| inner.is_null())
            && !required_keys.contains(&key.as_str())
            && repaired_list(root, &subs)
                .is_some_and(|child| single_type(&child).is_some_and(|kind| kind != "null"));
        if drop_null {
            map.remove(&key);
            repairs.push(Repair {
                path: at,
                fix: RepairFix::NullDropped,
            });
        } else if let Some(inner) = map.get_mut(&key) {
            repair_at(root, &subs, inner, &at, repairs);
        }
    }
}

/// `text` read as the single `kind`. Parsed JSON is kept only when it
/// passes the check against every schema that reaches the position.
fn read_string(
    root: &Value,
    reaching: &[&Value],
    kind: &str,
    text: &str,
    pointer: &str,
    repairs: &mut Vec<Repair>,
) -> Option<(Value, RepairFix)> {
    match kind {
        "integer" | "number" => {
            number(text, kind == "integer").map(|n| (Value::Number(n), RepairFix::StringToNumber))
        }
        "boolean" => match text {
            "true" => Some((Value::Bool(true), RepairFix::StringToBoolean)),
            "false" => Some((Value::Bool(false), RepairFix::StringToBoolean)),
            _ => None,
        },
        "array" | "object" => {
            let mut parsed: Value = serde_json::from_str(text).ok()?;
            if !is(&parsed, kind) {
                return None;
            }
            let mut inside = Vec::new();
            repair_at(root, reaching, &mut parsed, pointer, &mut inside);
            let mut errors = Vec::new();
            examine(root, reaching, &parsed, pointer, &mut errors);
            if !errors.is_empty() {
                return None;
            }
            repairs.append(&mut inside);
            Some((parsed, RepairFix::StringParsed))
        }
        _ => None,
    }
}

/// The one type every schema in `list` that has `type` names, when they
/// agree. A `type` array of several types, or two different types, is none.
fn single_type<'a>(list: &[&'a Value]) -> Option<&'a str> {
    let mut found: Option<&str> = None;
    for schema in list {
        if schema.get("type").is_none() {
            continue;
        }
        let kinds = types(schema);
        let [kind] = kinds.as_slice() else {
            return None;
        };
        if found.is_some_and(|prev| prev != *kind) {
            return None;
        }
        found = Some(*kind);
    }
    found
}

fn union_required<'a>(list: &[&'a Value]) -> Vec<&'a str> {
    list.iter().flat_map(|schema| required(schema)).collect()
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

fn property<'a>(schema: &'a Value, key: &str) -> Option<&'a Value> {
    schema.get("properties").and_then(|props| props.get(key))
}

fn place(pointer: &str) -> String {
    if pointer.is_empty() {
        "arguments".to_owned()
    } else {
        format!("`{pointer}`")
    }
}

/// A key as a JSON Pointer token.
fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
