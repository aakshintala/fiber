//! The `--resume` selector (`docs/invocation.md`, "Lifecycle"): a full
//! session id or any prefix of one that is unique among the project's
//! sessions.

use std::path::Path;

use contract::SessionId;

use crate::{EVENTS, Error};

/// Resolves `selector` to the session it names in `sessions`: a name that
/// equals it, or the one name it prefixes. A directory is a session only
/// when it holds `events.jsonl`. The selector is only compared with names
/// listed in `sessions`, never joined into a path, so nothing outside
/// `sessions` is read or returned, whatever the selector holds. An empty
/// selector would prefix every name, so it matches nothing.
pub fn resolve(
    sessions: &Path,
    selector: &str,
    _in_project: &dyn Fn(&str) -> bool,
) -> Result<SessionId, Error> {
    if selector.is_empty() {
        return Err(Error::NotFound(sessions.to_owned()));
    }
    let mut names: Vec<String> = Vec::new();
    let entries = match std::fs::read_dir(sessions) {
        Ok(entries) => entries,
        Err(_) => return Err(Error::NotFound(sessions.to_owned())),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().map(|k| k.is_dir()).unwrap_or(false)
            && entry.path().join(EVENTS).is_file()
        {
            names.push(name);
        }
    }
    if names.iter().any(|name| name == selector) {
        return Ok(SessionId(selector.to_owned()));
    }
    let mut matches: Vec<String> = names
        .into_iter()
        .filter(|name| name.starts_with(selector))
        .collect();
    matches.sort();
    match matches.len() {
        1 => Ok(SessionId(matches.remove(0))),
        0 => Err(Error::NotFound(sessions.to_owned())),
        _ => Err(Error::Ambiguous {
            selector: selector.to_owned(),
            matches,
        }),
    }
}
