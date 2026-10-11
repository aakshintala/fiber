//! The name a session was given (`docs/tools.md`, "Messaging other sessions"):
//! the latest `session_named` in its log, read from the end backwards.

use std::path::{Component, Path};

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

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;
