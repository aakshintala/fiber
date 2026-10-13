//! One client connection's writer thread (`docs/architecture.md`, "The
//! threads"): owns the Watcher and the socket stream, and writes the
//! connection's events.

use std::io::Write;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use contract::{Envelope, SCHEMA_VERSION, SessionId};
use serde_json::{Map, Value};

use super::level::{LEVEL, Switch, Writing, apply};
use crate::session::Gate;

/// A first subscribe's deferred response: the loop's answer to the
/// delivered switch, emitted before anything the seeded watcher holds.
/// `response` is bare acceptance, then a notice on failure; `after` is
/// written next (summary snapshots; empty for full).
pub(crate) struct Prelude {
    pub(crate) response: mpsc::Receiver<Vec<Envelope>>,
    pub(crate) after: Vec<Envelope>,
}

/// Wakes a connection's writer so it leaves `recv`.
pub(crate) const STOP: &str = "doors.stop";

pub(crate) fn spawn_writer(
    gate: Arc<Gate>,
    id: u64,
    watcher: log::Watcher,
    stream: Box<dyn Write + Send>,
    summary: bool,
    prelude: Option<Prelude>,
) -> mpsc::Sender<Switch> {
    gate.conns.begin_writer();
    let ended = Arc::clone(&gate);
    let failed = Arc::clone(&gate);
    let (tx, rx) = mpsc::channel();
    let (switch_tx, switches) = mpsc::channel();
    match thread::Builder::new()
        .name("writer".to_owned())
        .spawn(move || {
            let _end = WriterEnd(ended);
            // The handle is stored before this write can block, so close joins it.
            if rx.recv().is_err() {
                return;
            }
            let mut stream = stream;
            // The loop's answer to the delivered switch, before folded
            // history: bare acceptance, then a notice on failure, then
            // the summary snapshots. A cancelled wait means the reader
            // is gone; ending here lets the join proceed.
            if let Some(prelude) = prelude {
                let settled = match prelude.response.recv() {
                    Ok(settled) => settled,
                    Err(_) => return,
                };
                for line in settled.into_iter().chain(prelude.after) {
                    if write_line(stream.as_mut(), &line).is_err() {
                        return;
                    }
                }
            }
            if write_loop(watcher, stream, summary, switches) == Ended::Failed {
                // The watcher failed: the reader and socket stay open, so
                // the connection is shut and the client sees EOF after
                // every line before the one that failed.
                failed.conns.shut(id);
            }
        }) {
        Ok(handle) => {
            gate.conns.push_writer(id, handle);
            if let Ok(()) = tx.send(()) {}
        }
        Err(_) => gate.conns.end_writer(),
    }
    switch_tx
}

struct WriterEnd(Arc<Gate>);

impl Drop for WriterEnd {
    fn drop(&mut self) {
        self.0.conns.end_writer();
    }
}

/// How a writer ended: Failed on a watcher error, else Done. A failed
/// write means the client already went, and the log's end is the session
/// close path, which reaps on its own: neither shuts the connection.
#[derive(PartialEq, Eq)]
pub(crate) enum Ended {
    Done,
    Failed,
}

pub(crate) fn write_loop(
    watcher: log::Watcher,
    mut stream: Box<dyn Write + Send>,
    summary: bool,
    switches: mpsc::Receiver<Switch>,
) -> Ended {
    let mut writing = Writing {
        watcher,
        summary,
        cutoff: 0,
        written: None,
    };
    loop {
        let line = match writing.watcher.recv() {
            Ok(Some(line)) => line,
            Ok(None) => return Ended::Done,
            Err(_) => return Ended::Failed,
        };
        if line.kind == STOP {
            return Ended::Done;
        }
        if line.kind == LEVEL {
            if apply(&mut writing, &switches, stream.as_mut()).is_err() {
                return Ended::Failed;
            }
            continue;
        }
        if writing.summary && !summary_line(&line.kind) {
            continue;
        }
        if line.seq.is_some_and(|seq| seq.0 < writing.cutoff) {
            continue;
        }
        if write_line(stream.as_mut(), &line).is_err() {
            return Ended::Done;
        }
        if let Some(seq) = line.seq {
            writing.written = Some(seq.0);
        }
    }
}

fn summary_line(kind: &str) -> bool {
    matches!(
        kind,
        "session_status" | "extensions_loaded" | "command_accepted" | "command_rejected"
    )
}

pub(crate) fn write_line(stream: &mut dyn Write, line: &Envelope) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(line)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()
}

/// A control line for `session`, such as the one that stops its writer.
pub(crate) fn control_line(session: &SessionId, kind: &str, token: u64) -> Envelope {
    let mut payload = Map::new();
    payload.insert("token".to_owned(), Value::from(token));
    Envelope {
        kind: kind.to_owned(),
        session_id: session.clone(),
        ts: 0,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    }
}
