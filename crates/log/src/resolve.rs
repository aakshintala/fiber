//! The `--resume` selector (`docs/invocation.md`, "Lifecycle"): a full
//! session id or any prefix of one that is unique among the project's
//! sessions.

use std::path::Path;

use contract::SessionId;
use contract::events::Event;

use crate::{EVENTS, Error};

/// Resolves `selector` to the session it names in `sessions`: a name that
/// equals it, or the one name it prefixes. A directory is a session only
/// when its first complete line is `session_started` (`docs/events.md`,
/// "A directory is a session ...") and `in_project` accepts that line's
/// workspace, so a crash between `Log::create` and the first write leaves
/// nothing to resolve, and a slug shared by two workspaces (`docs/state.md`,
/// "Projects") never reaches another project's sessions. A missing, torn,
/// empty or unparseable first line is not a session, without failing the
/// lookup. The selector is only compared with names listed in `sessions`,
/// never joined into a path, so nothing outside `sessions` is read or
/// returned, whatever the selector holds. An empty selector would prefix
/// every name, so it matches nothing.
pub fn resolve(
    sessions: &Path,
    selector: &str,
    in_project: &dyn Fn(&str) -> bool,
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
            && is_session(&entry.path(), in_project)
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

/// Whether `dir` holds one of this project's sessions: its first complete
/// line parses and is `session_started` with a workspace `in_project`
/// accepts. Anything else is not a session, and never an error: an empty
/// log, a torn first line, or a first line that does not parse all read as
/// no first line at all.
fn is_session(dir: &Path, in_project: &dyn Fn(&str) -> bool) -> bool {
    let mut lines = match crate::lines(dir) {
        Ok(lines) => lines,
        Err(_) => return false,
    };
    let Some(first) = lines.next() else {
        return false;
    };
    let Ok(line) = first else {
        return false;
    };
    match Event::from_envelope(&line) {
        Ok(Some(Event::SessionStarted(started))) => in_project(&started.workspace),
        _ => false,
    }
}
