//! `directories` (`docs/invocation.md`, "What the hub speaks"): the names
//! of one directory's subdirectories, sorted, one level only, with the
//! absolute path listed, `~/` expanded, so a client starts a session in a
//! path it typed from `~/`. A path that is not a readable directory
//! answers an empty list; a relative path is rejected.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::feed::{Refusal, invalid};

/// `directories`: the names of the subdirectories `path` holds, sorted,
/// one level only, and `path`, the absolute path listed with `~/`
/// expanded. The answer is always `Some`. Only the argument shape and an
/// unusable home reject, always `invalid_arguments`: a filesystem error
/// answers `[]` with `path`.
pub(crate) fn directories(
    home: Option<&Path>,
    args: &Map<String, Value>,
) -> Result<Option<Value>, Refusal> {
    let raw = parse(args).ok_or_else(invalid)?;
    let path = expand(home, raw).ok_or_else(invalid)?;
    let mut names = Vec::new();
    if let Ok(entries) = fs::read_dir(&path) {
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()) {
                names.push(name);
            }
        }
    }
    names.sort();
    Ok(Some(json!({"directories": names, "path": path})))
}

/// The `path` argument: exactly one `path` key holding a string.
fn parse(args: &Map<String, Value>) -> Option<&str> {
    if args.keys().any(|key| key != "path") {
        return None;
    }
    args.get("path")?.as_str()
}

/// The absolute path `raw` lists: as given when absolute, or the home
/// with `~/` expanded. `~` alone and `~user/` are relative, as is every
/// other start but `/` and `~/`. A home that is missing, relative or not
/// UTF-8 expands nothing. Trimming never empties the path: home `/`
/// answers `/rest`.
fn expand(home: Option<&Path>, raw: &str) -> Option<String> {
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = home?;
        if !home.is_absolute() {
            return None;
        }
        let trimmed = home.to_str()?.trim_end_matches('/');
        if trimmed.is_empty() {
            return Some(format!("/{rest}"));
        }
        return Some(format!("{trimmed}/{rest}"));
    }
    if raw.starts_with('/') {
        return Some(raw.to_owned());
    }
    None
}

#[cfg(test)]
#[path = "directories_tests.rs"]
mod tests;
