//! `attention` hub lines (`docs/invocation.md`, "Attention"): the hub tells
//! every connected client when a top-level session needs the person, with a
//! `reason` of `waiting` or `finished`. The decision is [`reason`], a pure
//! function of the new status and the [`Facts`]; [`Attention`] holds the
//! listener registry, the hub's start and what it announced per session.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::feed::{Closer, Fanout, WriterEnd, close_connections};

use contract::clock::{Clock, wall_ms};
use contract::events::{SessionState, SessionStatus};
use contract::{HubLine, RequestId, SCHEMA_VERSION};
use serde_json::{Map, Value};
use support::lock;

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

/// The attention `now` calls for. None for a delegate, and no `finished`
/// for a supervised session: it stays in the feed, but a program drives
/// it, so it sends no attention line (`docs/permissions.md`, "Supervised
/// sessions"). Only `finished` is suppressed: `waiting` still reports, as
/// documented (`docs/invocation.md`, "Attention").
pub(crate) fn reason(now: &SessionStatus, facts: &Facts<'_>) -> Option<Reason> {
    if now.parent.is_some() {
        return None;
    }
    let reason = match &now.state {
        SessionState::Waiting { waiting } => {
            if facts.announced.request.as_ref() == Some(&waiting.request_id) {
                None
            } else {
                Some(Reason::Waiting)
            }
        }
        SessionState::Idle => {
            if !facts.fresh || facts.announced.since == Some(now.since) {
                None
            } else {
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
        }
        SessionState::Streaming
        | SessionState::Tool { .. }
        | SessionState::Retrying
        | SessionState::Jobs => None,
    };
    // A supervised session sends no `finished` attention; every other
    // shape reports as documented.
    if now.supervised && reason == Some(Reason::Finished) {
        None
    } else {
        reason
    }
}

/// Every connection's listener (a channel and writer thread each, in the
/// shared [`Fanout`] registry), the hub's start, and what it announced
/// per session.
pub(crate) struct Attention {
    clock: Arc<dyn Clock>,
    started_ms: u64,
    announced: Mutex<BTreeMap<String, Announced>>,
    listeners: Mutex<Fanout>,
    stopped: AtomicBool,
    /// Tests only: fails the next `listen` while live, so the
    /// refused-registration path runs without an outside seam: `listen`
    /// refuses only once stopped or when armed, and stopping the attention
    /// would end the live hub the test needs, while no socket, starter,
    /// clock or file seam fails one registration while live.
    #[cfg(test)]
    fail_listen: AtomicBool,
}

impl Attention {
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Self {
        let started_ms = wall_ms(clock.wall());
        Self {
            clock,
            started_ms,
            announced: Mutex::new(BTreeMap::new()),
            listeners: Mutex::new(Fanout::default()),
            stopped: AtomicBool::new(false),
            #[cfg(test)]
            fail_listen: AtomicBool::new(false),
        }
    }

    /// Reserves a pending listener: no channel and no writer thread until
    /// the first unsolicited line activates it, under the registry lock,
    /// at most once. Refused (`None`) once stopped.
    pub(crate) fn listen(
        &self,
        writer: Arc<Mutex<UnixStream>>,
        closer: Closer,
        retired: Closer,
    ) -> Option<u64> {
        #[cfg(test)]
        if self.fail_listen.swap(false, Ordering::SeqCst) {
            return None;
        }
        let mut listeners = lock(&self.listeners);
        if self.stopped.load(Ordering::SeqCst) {
            return None;
        }
        Some(listeners.listen(writer, closer, retired))
    }

    /// Queues one unsolicited hub line to every served connection, handing
    /// back the shutdown of every listener that already disconnected, for
    /// the caller to run outside every lock. Past a stop the line is
    /// dropped.
    pub(crate) fn announce(&self, line: crate::feed::Line) -> Vec<Closer> {
        if self.stopped.load(Ordering::SeqCst) {
            return Vec::new();
        }
        lock(&self.listeners).broadcast(&line)
    }

    /// Tests only: fails the next `listen` while live, so the
    /// refused-registration path runs without an outside seam: `listen`
    /// refuses only once stopped or when armed, and stopping the attention
    /// would end the live hub the test needs, while no socket, starter,
    /// clock or file seam fails one registration while live.
    #[cfg(test)]
    pub(crate) fn fail_next_listen(&self) {
        self.fail_listen.store(true, Ordering::SeqCst);
    }

    /// Drops listener `id`, reaps retired writers that have already
    /// ended, then shuts and joins every remaining retired writer. This
    /// closes a blocked writer even when overflow has not run its own
    /// shutdown hand-off yet. A pending listener owns no thread and shuts
    /// nothing. Every shutdown and join runs outside the registry lock.
    pub(crate) fn unlisten(&self, id: u64) {
        let (ends, finished) = {
            let mut listeners = lock(&self.listeners);
            let gone = listeners.remove(id);
            let finished = listeners.finished();
            let mut ends = listeners.take_retired();
            if let Some(gone) = gone {
                ends.push(gone);
            }
            (ends, finished)
        };
        close_and_join(ends);
        // Already ended, so joining needs no shutdown: `finished` dropped
        // each reaped closer and released its descriptor.
        for writer in finished {
            match writer.join() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Stops the registry for the feed's stop: no later listen registers
    /// and no later line is queued. Active writers shut before joining,
    /// so a blocked writer ends at once. Pending registrations own no
    /// thread and shut nothing: they are dropped, so a relay-only
    /// connection keeps receiving past the feed's stop.
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let mut old = std::mem::take(&mut *lock(&self.listeners));
        // Already-ended retired writers join without shutdown; the fast
        // path drops their closers. `close` drops pending registrations
        // unshut and returns every active or unfinished retired writer.
        let finished = old.finished();
        close_and_join(old.close());
        for writer in finished {
            match writer.join() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// The unseen-turn test is
    /// `now.since >= started_ms && tail holds turn_completed(now.since)`.
    /// Takes no lock.
    pub(crate) fn ended_unseen(&self, dir: &Path, now: &SessionStatus) -> bool {
        if now.since < self.started_ms {
            return false;
        }
        let since = now.since;
        log::tail_lines(dir).iter().any(|line| {
            let Ok(value) = serde_json::from_slice::<Value>(line) else {
                return false;
            };
            value.get("kind").and_then(Value::as_str) == Some("turn_completed")
                && value.get("ts").and_then(Value::as_u64) == Some(since)
        })
    }

    /// Decides through `reason`, updates `announced`, queues the line,
    /// handing back the shutdown of every listener that already
    /// disconnected, for the caller to run outside every lock.
    pub(crate) fn notify(
        &self,
        session: &str,
        before: Option<Seen<'_>>,
        now: &SessionStatus,
        unseen: bool,
    ) -> Vec<Closer> {
        if self.stopped.load(Ordering::SeqCst) {
            return Vec::new();
        }
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
                return Vec::new();
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
        lock(&self.listeners).broadcast(&line)
    }

    pub(crate) fn forget(&self, session: &str) {
        lock(&self.announced).remove(session);
    }

    /// How many attention writers started, including retired ones: actual
    /// successful starts, not currently live writers. Test-only: no
    /// outside socket outcome distinguishes an idle parked thread from no
    /// thread.
    #[cfg(test)]
    pub(crate) fn writers_started(&self) -> usize {
        lock(&self.listeners).started()
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

/// Closes active connections before joining their writers. Pending
/// registrations own no thread, so their shutdown handles are dropped.
fn close_and_join(ends: Vec<WriterEnd>) {
    let mut closers = Vec::new();
    let mut writers = Vec::new();
    for end in ends {
        if let Some(writer) = end.writer {
            closers.push(end.closer);
            writers.push(writer);
        }
    }
    close_connections(closers, UnixStream::shutdown);
    for writer in writers {
        match writer.join() {
            Ok(()) | Err(_) => {}
        }
    }
}

#[cfg(test)]
#[path = "attention_tests.rs"]
mod tests;
