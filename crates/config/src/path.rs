//! Dotted key paths, such as `models."openai/gpt-5.6".handoff.tokens`, and the
//! merge rule of `docs/configuration.md`, "Layers".

use serde_json::{Map, Value};

/// Splits a dotted path. A segment in double quotes may hold dots. `None` for
/// an empty path, an empty segment or an unclosed quote.
pub(crate) fn parse(text: &str) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut rest = text;
    loop {
        let (segment, after) = match rest.strip_prefix('"') {
            Some(quoted) => {
                let end = quoted.find('"')?;
                (quoted.get(..end)?, quoted.get(end + 1..)?)
            }
            None => rest.split_at(rest.find('.').unwrap_or(rest.len())),
        };
        if segment.is_empty() {
            return None;
        }
        segments.push(segment.to_owned());
        if after.is_empty() {
            return Some(segments);
        }
        rest = after.strip_prefix('.')?;
    }
}

/// The dotted form of a path, quoting a segment that holds a dot.
pub(crate) fn display(path: &[String]) -> String {
    path.iter()
        .map(|s| {
            if s.contains('.') {
                format!("\"{s}\"")
            } else {
                s.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The value at `path`, if every step is an object that holds the next.
pub(crate) fn get<'a>(root: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(root, |value, name| value.get(name))
}

/// Sets the value at `path`, making each missing or non-object step an
/// object.
pub(crate) fn set(root: &mut Value, path: &[String], value: Value) {
    match path.split_first() {
        None => *root = value,
        Some((name, rest)) => {
            let mut map = if let Value::Object(map) = root.take() {
                map
            } else {
                Map::new()
            };
            set(map.entry(name.clone()).or_insert(Value::Null), rest, value);
            *root = Value::Object(map);
        }
    }
}

/// Lays `upper` over `lower`: objects merge key by key, any other value,
/// a list included, replaces the one below it.
pub(crate) fn merge(lower: &mut Value, upper: &Value) {
    match (lower, upper) {
        (Value::Object(below), Value::Object(above)) => {
            for (name, value) in above {
                match below.get_mut(name) {
                    Some(existing) => merge(existing, value),
                    None => {
                        below.insert(name.clone(), value.clone());
                    }
                }
            }
        }
        (below, above) => *below = above.clone(),
    }
}
