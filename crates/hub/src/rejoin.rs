//! Rejoining a resumed session at the level the connection last held
//! (`docs/invocation.md`, "Driver commands", `subscribe` row): "Through
//! the hub, a session that resumes is subscribed at the level the
//! connection last held."
//!
//! When a session a hub connection held a level on resumes, the hub
//! reopens the relay and subscribes it at that level with no client
//! command, so the client receives the new run's `session_status` lines.
//! The sweep runs off the feed's rescan of `run/` (R3 in the ticket's
//! plan): on the scanner thread it only collects candidates, and a single
//! `hub-rejoin` worker reads the logs, connects and attaches.

#![allow(
    dead_code,
    reason = "the red commit holds inert signatures; later commits fill them"
)]

use std::collections::{BTreeSet, HashMap};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};

use serde_json::{Map, Value};

use crate::connection::{Hub, lock};
use crate::relay::Relays;

/// Per connection, in `Relays`: what the sweep needs.
#[derive(Default)]
pub(crate) struct Rejoin {
    /// One occurrence per `Opening` guard holding `session` open.
    opening: HashMap<String, usize>,
    /// Per session, the `Mark` of its last relay.
    marks: HashMap<String, Mark>,
    /// Set by `Relays::close_all`: the connection is gone.
    closed: bool,
}

/// Where `session`'s log stood when a relay to it opened, and what was read since.
#[derive(Clone)]
pub(crate) struct Mark {
    /// The session's log when a relay opened; `None` when it had none.
    log: Option<PathBuf>,
    /// Bytes consumed: always the end of a complete line, never a raw length.
    read: u64,
    /// Whether a `fiber_started` past the mark has no later end after it.
    live: bool,
    /// The relay epoch the mark was recorded at.
    epoch: u64,
}

impl Mark {
    /// `find_log`, then the end of its last complete line (one past the
    /// last `'\n'`; 0 when none or no log). Never the raw length: a torn
    /// tail is discarded, and a resumed writer truncates a partial line
    /// before appending (`docs/events.md`, "Writing"), so the resumed
    /// `fiber_started` can land at that line's start. File IO: never under
    /// the relays lock.
    pub(crate) fn now(home: &Path, session: &str) -> Mark {
        let _ = (home, session);
        Mark {
            log: None,
            read: 0,
            live: false,
            epoch: 0,
        }
    }
}

impl Rejoin {
    pub(crate) fn close(&mut self) {
        self.closed = true;
    }
}

/// Under the relays lock, in attach, after mint and before spawn.
/// Exclusive: false when the connection is closed, any relay for `session`
/// exists, or `session` is opening. Otherwise (and always when not
/// exclusive) records `mark` at `epoch` and returns true.
pub(crate) fn admit(
    held: &mut Relays,
    session: &str,
    exclusive: bool,
    mark: Mark,
    epoch: u64,
) -> bool {
    if exclusive
        && (held.rejoin.closed
            || held.entries.iter().any(|entry| entry.session == session)
            || held
                .rejoin
                .opening
                .get(session)
                .is_some_and(|open| *open > 0))
    {
        return false;
    }
    let mut mark = mark;
    mark.epoch = epoch;
    held.rejoin.marks.insert(session.to_owned(), mark);
    true
}

/// Writes `mark` back for `session` only when its epoch is the stored one:
/// a write-back for an older epoch changes nothing.
pub(crate) fn store_back(held: &mut Relays, session: &str, mark: Mark) {
    if held
        .rejoin
        .marks
        .get(session)
        .is_some_and(|stored| stored.epoch == mark.epoch)
    {
        held.rejoin.marks.insert(session.to_owned(), mark);
    }
}

/// Marks `session` opening on this connection; Drop removes one
/// occurrence, taking the relays lock: never dropped while this thread
/// holds that lock.
pub(crate) struct Opening {
    relays: Arc<Mutex<Relays>>,
    session: String,
}

impl Opening {
    pub(crate) fn mark(held: &mut Relays, relays: &Arc<Mutex<Relays>>, session: &str) -> Opening {
        *held.rejoin.opening.entry(session.to_owned()).or_insert(0) += 1;
        Opening {
            relays: Arc::clone(relays),
            session: session.to_owned(),
        }
    }
}

impl Drop for Opening {
    fn drop(&mut self) {
        let mut held = lock(&self.relays);
        match held.rejoin.opening.get_mut(&self.session) {
            Some(open) if *open > 1 => *open -= 1,
            _ => {
                held.rejoin.opening.remove(&self.session);
            }
        }
    }
}

/// One sweep candidate: a kept subscription whose session may have resumed.
pub(crate) struct Candidate {
    session: String,
    mark: Mark,
    kept: Map<String, Value>,
    writer: Arc<Mutex<UnixStream>>,
    relays: Arc<Mutex<Relays>>,
}

/// What one sweep collects on the scanner thread, with no IO: each live
/// connection's sessions with a kept subscription, no relay, a mark, a name
/// in `names`, not opening and not closed.
pub(crate) fn candidates(
    held: &Relays,
    names: &BTreeSet<String>,
) -> Vec<(String, Mark, Map<String, Value>)> {
    if held.rejoin.closed {
        return Vec::new();
    }
    held.subscribed
        .iter()
        .filter_map(|(session, kept)| {
            if !names.contains(session) {
                return None;
            }
            if held.entries.iter().any(|entry| entry.session == *session) {
                return None;
            }
            if held
                .rejoin
                .opening
                .get(session)
                .is_some_and(|open| *open > 0)
            {
                return None;
            }
            let mark = held.rejoin.marks.get(session)?.clone();
            Some((session.clone(), mark, kept.clone()))
        })
        .collect()
}

/// Hub-wide: every served connection's writer and relays, held weakly.
#[derive(Default)]
pub(crate) struct Connections {
    inner: Mutex<Inner>,
    /// Tests only: a one-shot pause the worker runs before each connect.
    #[cfg(test)]
    pub(crate) before_connect: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: told at the end of every worker pass.
    #[cfg(test)]
    pub(crate) pass_done: Mutex<Option<mpsc::Sender<()>>>,
}

/// One served connection as the sweep holds it: its number, its writer
/// and its relays, the last two weakly so a gone connection drops out.
type Held = (u64, Weak<Mutex<UnixStream>>, Weak<Mutex<Relays>>);

#[derive(Default)]
struct Inner {
    conns: Vec<Held>,
    busy: bool,
}

impl Connections {
    pub(crate) fn register(
        &self,
        n: u64,
        writer: &Arc<Mutex<UnixStream>>,
        relays: &Arc<Mutex<Relays>>,
    ) {
        lock(&self.inner)
            .conns
            .push((n, Arc::downgrade(writer), Arc::downgrade(relays)));
    }

    pub(crate) fn unregister(&self, n: u64) {
        lock(&self.inner).conns.retain(|(id, _, _)| *id != n);
    }

    /// The registered connection numbers, pruning dead entries first.
    #[cfg(test)]
    pub(crate) fn ids(&self) -> Vec<u64> {
        let mut inner = lock(&self.inner);
        inner
            .conns
            .retain(|(_, writer, relays)| writer.upgrade().is_some() && relays.upgrade().is_some());
        inner.conns.iter().map(|(id, _, _)| *id).collect()
    }
}

/// Sets `hub.feed.on_scan`, holding the hub weakly; a dropped hub makes the hook a no-op.
pub(crate) fn wire(hub: &Arc<Hub>) {
    let weak = Arc::downgrade(hub);
    hub.feed.on_scan.get_or_init(|| {
        Box::new(move |names: &BTreeSet<String>| {
            if let Some(hub) = weak.upgrade() {
                sweep(&hub, names);
            }
        })
    });
}

/// On the feed's scanner thread, no IO. When no worker runs, collects each
/// live connection's candidates under its relays lock: a kept
/// subscription, no relay, a mark, a name in `names`, not opening, not
/// closed. When any are found, sets busy and spawns `hub-rejoin` running
/// `rejoin_pass`. A failed spawn clears busy.
fn sweep(hub: &Arc<Hub>, names: &BTreeSet<String>) {
    let _ = (hub, names);
}

/// On the worker, with no lock held across IO. For each candidate:
/// advances its mark and stores it back for its epoch only. For a live one
/// whose socket accepts: `relay::attach` with the kept replay, exclusive.
/// A guard clears busy at the end (panic included). In tests it runs the
/// pause hook before each connect and signals pass-done at the end.
fn rejoin_pass(hub: &Arc<Hub>, candidates: Vec<Candidate>) {
    let _ = (hub, candidates);
}

/// Reads complete lines from `mark.read`: `fiber_started` makes it live;
/// `fiber_exited` or `rewound` ends it; read moves past the last complete
/// line only, so a partial tail is read again next time. A file shorter
/// than `read` was replaced: resets read to 0 and live to false, then
/// reads the new file from its start.
fn advance(mark: Mark) -> Mark {
    mark
}

/// The kept subscription for a candidate whose stored mark is still the
/// one it was collected with. Inert until the sweep checks epochs.
#[allow(dead_code, reason = "inert until the sweep checks epochs")]
pub(crate) fn kept_for_candidate(
    held: &Relays,
    session: &str,
    candidate_epoch: u64,
) -> Option<Map<String, Value>> {
    let _ = (held, session, candidate_epoch);
    None
}

#[cfg(test)]
#[path = "rejoin_tests.rs"]
mod tests;
