//! Editing several entries of one object in Fiber home's `config.json`
//! in a single locked write (`docs/configuration.md`, "When Fiber writes"):
//! the `/keys` screen saves only the bindings that differ from the
//! defaults.

use std::path::Path;

use serde_json::{Map, Value};

use crate::Source;
use crate::error::ConfigError;
use crate::home::read;

use super::{checked, locked, write_root};

/// Sets each `Some` entry and removes each `None` entry of the object at
/// `key` in Fiber home's `config.json`: one lock, one read, one write. An
/// absent file reads as an empty object, and an absent object starts
/// empty. Removing the last entry removes `key` itself. Other keys, and
/// the object's other entries, are kept as they are. A `key` that holds
/// no object is refused, and the result is type-checked as [`set_global`]
/// checks it, so on any `Err` the file is unchanged. A call that changes
/// nothing writes nothing.
pub fn update_global_entries(
    home: &Path,
    key: &str,
    entries: &[(String, Option<Value>)],
) -> Result<(), ConfigError> {
    let file = home.join("config.json");
    let _lock = locked(&file)?;
    let mut root = read(&file)?.unwrap_or_else(|| Value::Object(Map::new()));
    let held: Option<Map<String, Value>> = match root.get(key) {
        None => None,
        Some(Value::Object(map)) => Some(map.clone()),
        Some(_) => {
            return Err(ConfigError::WrongType {
                source_name: file.display().to_string(),
                key: key.to_owned(),
                expected: "an object".to_owned(),
            });
        }
    };
    let mut table = held.clone().unwrap_or_default();
    for (name, value) in entries {
        match value {
            Some(value) => {
                table.insert(name.clone(), value.clone());
            }
            None => {
                table.remove(name);
            }
        }
    }
    if Some(&table) == held.as_ref() || (table.is_empty() && held.is_none()) {
        return Ok(());
    }
    if table.is_empty() {
        let _ = crate::path::remove(&mut root, &[key.to_owned()]);
    } else {
        crate::path::set(&mut root, &[key.to_owned()], Value::Object(table.clone()));
        checked(key, Value::Object(table), &Source::Global(file.clone()))?;
    }
    write_root(&file, &root)?;
    Ok(())
}

#[cfg(test)]
#[path = "entries_tests.rs"]
mod tests;
