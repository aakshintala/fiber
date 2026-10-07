//! The project's prompt history (`docs/state.md`, "Prompt history"): one
//! JSON line per prompt a person sends a hub-started session, appended to
//! `history.jsonl` beside the project's `sessions/`.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use contract::SessionId;
use contract::shapes::ContentPart;

/// The history file of the project whose session directory is `dir`
/// (`projects/<key>/sessions/<id>/`): `projects/<key>/history.jsonl`.
pub(crate) fn path(dir: &Path) -> Option<PathBuf> {
    Some(dir.parent()?.parent()?.join("history.jsonl"))
}

/// Appends one prompt to `path` as one line, in one write, so concurrent
/// sessions never interleave (`docs/state.md`, "Concurrent access").
pub(crate) fn append(
    path: &Path,
    ts_ms: u64,
    session: &SessionId,
    content: &[ContentPart],
) -> io::Result<()> {
    let invalid = |e| io::Error::new(io::ErrorKind::InvalidData, e);
    let session = serde_json::to_string(&session.0).map_err(invalid)?;
    let content = serde_json::to_string(content).map_err(invalid)?;
    // Built whole before the write, keys in the order the doc gives them.
    let line = format!("{{\"ts\":{ts_ms},\"session_id\":{session},\"content\":{content}}}\n");
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?
        .write_all(line.as_bytes())
}

#[cfg(test)]
#[path = "prompt_history_tests.rs"]
mod tests;
