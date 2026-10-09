//! The `--resume` selector (`docs/invocation.md`, "Lifecycle"): a full
//! session id or any prefix of one that is unique among the project's
//! sessions.

use std::path::Path;

use contract::SessionId;
use contract::events::Event;

use crate::{EVENTS, Error};

/// The session used most recently in `sessions`
/// (`docs/invocation.md`, "Commands and flags"): the one whose log's
/// last line is newest, live or exited alike. A directory counts only
/// when [`resolve`] would accept it, and never when its first line names
/// a parent: a delegate resumes only through its parent
/// (`docs/delegates.md`), which the hub refuses to resume. Activity is
/// the `ts` of the log's last complete line; when that line does not
/// parse, or holds no numeric `ts`, the first line's `ts` stands in, so
/// a session is never skipped for a corrupt tail. Ties go to the greater
/// id. A missing `sessions` directory holds no candidate.
pub fn most_recent(sessions: &Path, in_project: &dyn Fn(&str) -> bool) -> Option<SessionId> {
    let entries = std::fs::read_dir(sessions).ok()?;
    let mut best: Option<(u64, SessionId)> = None;
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false)
            || !entry.path().join(EVENTS).is_file()
        {
            continue;
        }
        let Some(opening) = opening(&entry.path(), in_project) else {
            continue;
        };
        if opening.delegate {
            continue;
        }
        let activity = crate::last_ts(&entry.path()).unwrap_or(opening.ts);
        let id = SessionId(entry.file_name().to_string_lossy().into_owned());
        let newer = match &best {
            None => true,
            Some((ts, winner)) => (activity, &id) > (*ts, winner),
        };
        if newer {
            best = Some((activity, id));
        }
    }
    best.map(|(_, id)| id)
}

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
            && opening(&entry.path(), in_project).is_some()
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

/// What `dir`'s first complete line says, when it holds one of this
/// project's sessions: its `ts`, and whether it names a parent, which
/// makes the session a delegate (`docs/delegates.md`). `None` when it
/// holds none: an empty log, a torn first line, a first line that does
/// not parse or is not `session_started`, or a workspace `in_project`
/// refuses.
struct Opening {
    /// The first line's `ts`.
    ts: u64,
    /// The first line names a parent.
    delegate: bool,
}

fn opening(dir: &Path, in_project: &dyn Fn(&str) -> bool) -> Option<Opening> {
    let mut lines = crate::lines(dir).ok()?;
    let first = lines.next()?;
    let line = first.ok()?;
    match Event::from_envelope(&line) {
        Ok(Some(Event::SessionStarted(started))) if in_project(&started.workspace) => {
            Some(Opening {
                ts: line.ts,
                delegate: started.parent.is_some(),
            })
        }
        _ => None,
    }
}
