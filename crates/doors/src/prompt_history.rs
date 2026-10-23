//! The project's prompt history (`docs/state.md`, "Prompt history"): one
//! JSON line per prompt a person sends a hub-started session, appended to
//! `history.jsonl` beside the project's `sessions/`.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use contract::clock::wall_ms;
use contract::inbox::Ack;
use contract::shapes::ContentPart;
use contract::{CommandId, SessionId};
use support::lock;

use crate::session::Gate;

/// The history file of the project whose session directory is `dir`
/// (`projects/<key>/sessions/<id>/`): `projects/<key>/history.jsonl`.
pub(crate) fn path(dir: &Path) -> Option<PathBuf> {
    Some(dir.parent()?.parent()?.join("history.jsonl"))
}

/// A prompt's content waiting for its acknowledgement, by command id: the
/// loop's `on_rewrite` replaces it before the prompt is accepted, and the
/// recording acknowledgement takes it. An entry lives only between a
/// prompt's delivery and its acknowledgement, called or dropped, so a
/// rejected command id a client retries registers a new entry the old
/// acknowledgement never touches.
pub(crate) struct Pending(Mutex<HashMap<CommandId, Vec<ContentPart>>>);

impl Pending {
    /// No prompt waiting.
    pub(crate) fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    /// Remembers `content` for `id`.
    pub(crate) fn register(&self, id: CommandId, content: Vec<ContentPart>) {
        lock(&self.0).insert(id, content);
    }

    /// Replaces the entry for `id`, when one is pending: never inserts, so
    /// a rewrite for an unknown prompt, a steer or a settled one changes
    /// nothing.
    pub(crate) fn replace(&self, id: &CommandId, content: Vec<ContentPart>) {
        if let Some(entry) = lock(&self.0).get_mut(id) {
            *entry = content;
        }
    }

    /// Takes the entry for `id`, when one is pending.
    pub(crate) fn take(&self, id: &CommandId) -> Option<Vec<ContentPart>> {
        lock(&self.0).remove(id)
    }

    /// Whether an entry for `id` is pending.
    #[cfg(test)]
    pub(crate) fn contains(&self, id: &CommandId) -> bool {
        lock(&self.0).contains_key(id)
    }
}

/// Wraps a prompt's acknowledgement so an accepted prompt is appended to
/// the prompt history at `path` before `ack` answers, so a client that
/// reads its acceptance finds the line. The pending entry is taken before
/// the inner ack is answered or dropped. A wrapper dropped uncalled drops
/// `ack`, which still answers `closing`.
pub(crate) fn recorded(ack: Ack, path: PathBuf, gate: &Arc<Gate>, id: CommandId) -> Ack {
    let mut recorded = Recorded {
        gate: Arc::clone(gate),
        id,
        path,
        inner: Some(ack),
    };
    Ack(Box::new(move |result| {
        let entry = recorded.gate.prompts.take(&recorded.id);
        if result.is_ok()
            && let Some(content) = entry
        {
            // debt: a failed append is dropped unreported, when a session gets a diag log, record a failed history append.
            let ts = wall_ms(recorded.gate.clock.wall());
            match append(&recorded.path, ts, &recorded.gate.session_id, &content) {
                Ok(()) | Err(_) => {}
            }
        }
        if let Some(inner) = recorded.inner.take() {
            (inner.0)(result);
        }
    }))
}

/// The acknowledgement [`recorded`] wraps, with the entry its call removes:
/// dropping it uncalled removes the entry first and only then drops the
/// inner ack.
struct Recorded {
    gate: Arc<Gate>,
    id: CommandId,
    path: PathBuf,
    inner: Option<Ack>,
}

impl Drop for Recorded {
    fn drop(&mut self) {
        if self.inner.is_some() {
            self.gate.prompts.take(&self.id);
        }
        drop(self.inner.take());
    }
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
