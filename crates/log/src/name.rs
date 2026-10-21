//! The name a session was given (`docs/tools.md`, "Messaging other sessions"):
//! the latest `session_named` in its log, read from the end backwards.

use std::io::BufRead as _;
use std::path::{Component, Path, PathBuf};

use contract::SessionId;
use serde_json::Value;

/// The latest `session_named` name in session `id`'s log under `home`,
/// read from the end backwards (`docs/tools.md`, "Messaging other
/// sessions"). `None` when the id is not one normal path component, the
/// directory is missing or a symlink, the log has no `session_named`, the
/// latest one cleared the name, or the log cannot be read.
pub fn session_name(home: &Path, id: &SessionId) -> Option<String> {
    // Checked before any path is built from it, so a path never leaves
    // `sessions/`.
    let mut parts = Path::new(&id.0).components();
    if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
        return None;
    }
    let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
        return None;
    };
    for project in projects.flatten() {
        let dir = project.path().join("sessions").join(&id.0);
        // `symlink_metadata` does not follow a link: a linked directory is
        // not a session.
        if !std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_dir()) {
            continue;
        }
        if let found @ Some(_) = crate::scan::last_line_where(&dir, |line| {
            let event: Value = serde_json::from_slice(line).ok()?;
            if event.get("kind")?.as_str()? != "session_named" {
                return None;
            }
            match event.get("payload")?.get("name") {
                // Cleared: the sender goes by its id alone.
                Some(Value::Null) => Some(None),
                Some(Value::String(name)) => Some(Some(name.clone())),
                // Missing or another type: not a name, read on.
                Some(_) | None => None,
            }
        }) {
            return found.flatten();
        }
    }
    None
}

/// The session directories for `id` under `home`: one per project, in
/// `read_dir` order, keeping only real directories. `None` when the id
/// is not one normal path component, so a path never leaves `sessions/`,
/// or when the projects directory cannot be read.
fn session_dirs<'a>(
    home: &'a Path,
    id: &'a SessionId,
) -> Option<impl Iterator<Item = PathBuf> + 'a> {
    // Checked before any path is built from it, so a path never leaves
    // `sessions/`.
    let mut parts = Path::new(&id.0).components();
    if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
        return None;
    }
    let projects = std::fs::read_dir(home.join("projects")).ok()?;
    Some(
        projects
            .flatten()
            .map(|project| project.path().join("sessions").join(&id.0))
            .filter(|dir| {
                // `symlink_metadata` does not follow a link: a linked directory is
                // not a session.
                std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_dir())
            }),
    )
}

/// The sender's parent session in session `id`'s log under `home`: the
/// first `session_started`'s `parent.session_id` (`docs/delegates.md`).
/// `None` under the same unreadable-log conditions as [`session_name`],
/// and when the session is top-level or its first line names no parent.
/// The log is the only authority for what happened in a session
/// (`docs/adr/0001-session-log-is-the-only-state-of-record.md`), which
/// covers fresh delegates, resumed delegates and top-level sessions
/// uniformly.
pub fn session_parent(home: &Path, id: &SessionId) -> Option<SessionId> {
    for dir in session_dirs(home, id)? {
        if let found @ Some(_) = first_parent(&dir) {
            return found.flatten();
        }
    }
    None
}

/// The `parent.session_id` of the first `session_started` line in `dir`'s
/// log: `Some(None)` when the first line starts a session with no parent,
/// `None` when the log cannot be read or starts with anything else. Bytes
/// without a closing newline are a torn write and read as nothing, as the
/// backwards scan treats them.
fn first_parent(dir: &Path) -> Option<Option<SessionId>> {
    let file = crate::open_log(dir).ok()?;
    let mut first = Vec::new();
    std::io::BufReader::new(file)
        .read_until(b'\n', &mut first)
        .ok()?;
    if first.is_empty() || first.last() != Some(&b'\n') {
        return None;
    }
    let view = crate::scan::SessionStarted::parse(&first)?;
    Some(view.parent())
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;
