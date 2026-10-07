//! `attention` hub lines (`docs/invocation.md`, "Attention"): the hub tells
//! every connected client when a top-level session needs the person, with a
//! `reason` of `waiting` or `finished`. The decision is [`reason`], a pure
//! function of the new status and the [`Facts`]; [`Attention`] holds the
//! listener registry, the hub's start and what it announced per session.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::net::UnixStream;
use std::path::Path;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use contract::clock::{Clock, wall_ms};
use contract::events::{SessionState, SessionStatus};
use contract::{HubLine, RequestId, SCHEMA_VERSION};
use serde_json::{Map, Value};

/// Why the hub tells clients a session needs the person (closed set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    Waiting,
    Finished,
}

impl Reason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Reason::Waiting => "waiting",
            Reason::Finished => "finished",
        }
    }
}

/// The last status the feed holds for a session. `live` is true for a running
/// entry, read on the summary connection still open, and false for a left
/// (crashed or exited) entry.
#[derive(Clone, Copy)]
pub(crate) struct Seen<'a> {
    pub(crate) status: &'a SessionStatus,
    pub(crate) live: bool,
}

/// What this hub last announced for a session.
#[derive(Clone, Default)]
pub(crate) struct Announced {
    pub(crate) request: Option<RequestId>,
    pub(crate) since: Option<u64>,
}

/// What `reason` decides on, besides the status itself.
pub(crate) struct Facts<'a> {
    pub(crate) before: Option<Seen<'a>>,
    /// `ended_unseen`'s answer: used only when `before` is not live.
    pub(crate) unseen: bool,
    /// `now.since >= started_ms`.
    pub(crate) fresh: bool,
    pub(crate) announced: &'a Announced,
}

/// The attention `now` calls for. None for a delegate.
pub(crate) fn reason(now: &SessionStatus, facts: &Facts<'_>) -> Option<Reason> {
    if now.parent.is_some() {
        return None;
    }
    match &now.state {
        SessionState::Waiting { waiting } => {
            if facts.announced.request.as_ref() == Some(&waiting.request_id) {
                None
            } else {
                Some(Reason::Waiting)
            }
        }
        SessionState::Idle => {
            if !facts.fresh || facts.announced.since == Some(now.since) {
                return None;
            }
            match facts.before {
                Some(seen) if seen.live => match &seen.status.state {
                    SessionState::Streaming
                    | SessionState::Tool { .. }
                    | SessionState::Retrying
                    | SessionState::Waiting { .. } => Some(Reason::Finished),
                    SessionState::Jobs | SessionState::Idle => None,
                },
                _ => {
                    if facts.unseen {
                        Some(Reason::Finished)
                    } else {
                        None
                    }
                }
            }
        }
        SessionState::Streaming
        | SessionState::Tool { .. }
        | SessionState::Retrying
        | SessionState::Jobs => None,
    }
}

/// Whether the last `TAIL` bytes of `log` hold a complete `turn_completed`
/// line whose integer `ts` equals `since`. False when `log` cannot be read.
pub(crate) fn turn_ended_at(log: &Path, since: u64) -> bool {
    let Ok(mut file) = File::open(log) else {
        return false;
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return false;
    };
    let from = len.saturating_sub(crate::feed::TAIL);
    // Whether the window starts mid-line: only then is its first line
    // partial. A window starting exactly after a newline starts at a
    // complete line, which counts.
    let mid_line = if from > 0 {
        let mut probe = [0; 1];
        if file
            .seek(SeekFrom::Start(from - 1))
            .and_then(|_| file.read_exact(&mut probe))
            .is_err()
        {
            return false;
        }
        probe[0] != b'\n'
    } else {
        false
    };
    if file.seek(SeekFrom::Start(from)).is_err() {
        return false;
    }
    let mut tail = Vec::new();
    if file.read_to_end(&mut tail).is_err() {
        return false;
    }
    // The tail's complete lines: a last line with no trailing newline is
    // torn and dropped, as is a partial first line.
    let mut parts = tail.split(|byte| *byte == b'\n');
    parts.next_back();
    if mid_line {
        parts.next();
    }
    parts.filter(|line| !line.is_empty()).any(|line| {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return false;
        };
        value.get("kind").and_then(Value::as_str) == Some("turn_completed")
            && value.get("ts").and_then(Value::as_u64) == Some(since)
    })
}

struct Listener {
    id: u64,
    tx: Sender<crate::feed::Line>,
    writer: JoinHandle<()>,
}

#[derive(Default)]
struct Listeners {
    next: u64,
    list: Vec<Listener>,
}

/// Every connection's listener (a channel and writer thread each), the hub's start,
/// and what it announced per session.
pub(crate) struct Attention {
    clock: Arc<dyn Clock>,
    started_ms: u64,
    announced: Mutex<BTreeMap<String, Announced>>,
    listeners: Mutex<Listeners>,
    #[cfg(test)]
    joining: AtomicUsize,
}

impl Attention {
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Self {
        let started_ms = wall_ms(clock.wall());
        Self {
            clock,
            started_ms,
            announced: Mutex::new(BTreeMap::new()),
            listeners: Mutex::new(Listeners::default()),
            #[cfg(test)]
            joining: AtomicUsize::new(0),
        }
    }

    pub(crate) fn listen(&self, writer: Arc<Mutex<UnixStream>>) -> Option<u64> {
        let (tx, handle) = crate::feed::spawn_writer(writer, "hub-attention-out")?;
        let mut listeners = lock(&self.listeners);
        listeners.next += 1;
        let id = listeners.next;
        listeners.list.push(Listener {
            id,
            tx,
            writer: handle,
        });
        Some(id)
    }

    /// Drops listener `id` and joins its writer, after releasing the lock.
    pub(crate) fn unlisten(&self, id: u64) {
        let gone = {
            let mut listeners = lock(&self.listeners);
            listeners
                .list
                .iter()
                .position(|listener| listener.id == id)
                .map(|at| listeners.list.remove(at))
        };
        if let Some(listener) = gone {
            drop(listener.tx);
            self.join(listener.writer);
        }
    }

    fn join(&self, handle: JoinHandle<()>) {
        #[cfg(test)]
        self.joining.fetch_add(1, Ordering::SeqCst);
        match handle.join() {
            Ok(()) | Err(_) => {}
        }
    }

    /// The unseen-turn test is
    /// `now.since >= started_ms && turn_ended_at(log, now.since)`. Takes no lock.
    pub(crate) fn ended_unseen(&self, log: &Path, now: &SessionStatus) -> bool {
        now.since >= self.started_ms && turn_ended_at(log, now.since)
    }

    /// Decides through `reason`, updates `announced`, queues the line.
    pub(crate) fn notify(
        &self,
        session: &str,
        before: Option<Seen<'_>>,
        now: &SessionStatus,
        unseen: bool,
    ) {
        let line = {
            let mut announced = lock(&self.announced);
            let known = announced.get(session).cloned().unwrap_or_default();
            let facts = Facts {
                before,
                unseen,
                fresh: now.since >= self.started_ms,
                announced: &known,
            };
            let Some(reason) = reason(now, &facts) else {
                return;
            };
            let line = line_for(session, now, reason, wall_ms(self.clock.wall()));
            let entry = announced.entry(session.to_owned()).or_default();
            match reason {
                Reason::Waiting => {
                    if let SessionState::Waiting { waiting } = &now.state {
                        entry.request = Some(waiting.request_id.clone());
                    }
                }
                Reason::Finished => {
                    entry.since = Some(now.since);
                }
            }
            line
        };
        let mut listeners = lock(&self.listeners);
        listeners
            .list
            .retain(|listener| listener.tx.send(Arc::clone(&line)).is_ok());
    }

    pub(crate) fn forget(&self, session: &str) {
        lock(&self.announced).remove(session);
    }

    #[cfg(test)]
    pub(crate) fn listeners(&self) -> usize {
        lock(&self.listeners).list.len()
    }

    #[cfg(test)]
    pub(crate) fn joining(&self) -> usize {
        self.joining.load(Ordering::SeqCst)
    }

    /// How many listeners' writers have returned.
    #[cfg(test)]
    pub(crate) fn ended(&self) -> usize {
        lock(&self.listeners)
            .list
            .iter()
            .filter(|listener| listener.writer.is_finished())
            .count()
    }
}

/// The `attention` hub line for `session`: `summary` only for `waiting`.
fn line_for(session: &str, now: &SessionStatus, reason: Reason, ts: u64) -> crate::feed::Line {
    let mut payload = Map::new();
    payload.insert("name".to_owned(), Value::String(now.name.clone()));
    payload.insert(
        "reason".to_owned(),
        Value::String(reason.as_str().to_owned()),
    );
    payload.insert("session_id".to_owned(), Value::String(session.to_owned()));
    if reason == Reason::Waiting
        && let SessionState::Waiting { waiting } = &now.state
    {
        payload.insert("summary".to_owned(), Value::String(waiting.summary.clone()));
    }
    payload.insert("workspace".to_owned(), Value::String(now.workspace.clone()));
    crate::feed::to_line(&HubLine {
        kind: "attention".to_owned(),
        ts,
        schema_version: SCHEMA_VERSION,
        payload,
    })
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "attention_tests.rs"]
mod tests;
