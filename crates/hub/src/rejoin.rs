//! Rejoining a resumed session at the level the connection last held
//! (`docs/invocation.md`, "Driver commands", `subscribe` row): "Through
//! the hub, a session that resumes is subscribed at the level the
//! connection last held."
//!
//! When a session a hub connection held a level on resumes, the hub
//! reopens the relay and subscribes it at that level with no client
//! command, so the client receives the new run's `session_status` lines.
//! The sweep runs off the feed's rescan of `run/`: on the scanner thread
//! it only collects candidates, and a single `hub-rejoin` worker reads the
//! logs, connects and attaches.

use std::collections::{BTreeSet, HashMap};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::thread;

use serde_json::{Map, Value};

use crate::connection::{Hub, lock};
use crate::feed::kind_of;
use crate::relay::Relays;

/// Per connection, in `Relays`: what the sweep needs.
#[derive(Default)]
pub(crate) struct Rejoin {
    /// One occurrence per `Opening` guard holding `session` open;
    /// never zero: `Drop` removes the last, so presence means opening.
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
        let log = crate::resume::find_log(home, &contract::SessionId(session.to_owned()));
        let read = log.as_deref().map_or(0, complete_len);
        Mark {
            log,
            read,
            live: false,
            epoch: 0,
        }
    }
}

/// The log `session` has now, for a mark that recorded none at attach:
/// its path with offset 0, so the later run's `fiber_started` counts.
/// Never `Mark::now`: that would place the offset after the start the
/// resumed run already wrote. File IO: never under the relays lock.
fn discovered(home: &Path, session: &str, epoch: u64) -> Option<Mark> {
    let log = crate::resume::find_log(home, &contract::SessionId(session.to_owned()))?;
    Some(Mark {
        log: Some(log),
        read: 0,
        live: false,
        epoch,
    })
}

impl Rejoin {
    pub(crate) fn close(&mut self) {
        self.closed = true;
    }
}

/// The end of the last complete line in the file at `log`: one past its
/// last newline, or 0 when it has none or cannot be read. Never the raw
/// length: a torn tail is discarded.
fn complete_len(log: &Path) -> u64 {
    let bytes = std::fs::read(log).unwrap_or_default();
    bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at as u64 + 1)
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
            || held.rejoin.opening.contains_key(session))
    {
        return false;
    }
    let mut mark = mark;
    mark.epoch = epoch;
    held.rejoin.marks.insert(session.to_owned(), mark);
    true
}

/// The current subscription for a candidate whose mark has not changed and
/// which can still be admitted exclusively. Call under the relays lock and
/// keep that lock through `admit` so the epoch and level cannot change between
/// this check and the admission.
pub(crate) fn kept_for_candidate(
    held: &Relays,
    session: &str,
    candidate_epoch: u64,
) -> Option<Map<String, Value>> {
    if held.rejoin.closed
        || held.entries.iter().any(|entry| entry.session == session)
        || held.rejoin.opening.contains_key(session)
        || !held
            .rejoin
            .marks
            .get(session)
            .is_some_and(|stored| stored.epoch == candidate_epoch)
    {
        return None;
    }
    held.subscription(session)
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
    writer: Arc<Mutex<UnixStream>>,
    relays: Arc<Mutex<Relays>>,
}

/// What one sweep collects on the scanner thread, with no IO: each live
/// connection's sessions with a kept subscription, no relay, a mark, a name
/// in `names`, not opening and not closed.
pub(crate) fn candidates(held: &Relays, names: &BTreeSet<String>) -> Vec<(String, Mark)> {
    if held.rejoin.closed {
        return Vec::new();
    }
    held.subscribed
        .iter()
        .filter_map(|(session, _kept)| {
            if !names.contains(session) {
                return None;
            }
            if held.entries.iter().any(|entry| entry.session == *session) {
                return None;
            }
            if held.rejoin.opening.contains_key(session) {
                return None;
            }
            let mark = held.rejoin.marks.get(session)?.clone();
            Some((session.clone(), mark))
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

/// One live connection as the sweep holds it across the scan: its writer
/// and its relays.
type Live = (Arc<Mutex<UnixStream>>, Arc<Mutex<Relays>>);

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
    if lock(&hub.rejoins.inner).busy {
        return;
    }
    // The live connections, pruning dead ones: the registry lock is never
    // held while a relays lock is taken.
    let live: Vec<Live> = {
        let mut inner = lock(&hub.rejoins.inner);
        let mut live = Vec::new();
        inner.conns.retain(
            |(_, writer, relays)| match (writer.upgrade(), relays.upgrade()) {
                (Some(writer), Some(relays)) => {
                    live.push((writer, relays));
                    true
                }
                _ => false,
            },
        );
        live
    };
    let mut found = Vec::new();
    for (writer, relays) in live {
        let held = lock(&relays);
        found.extend(
            candidates(&held, names)
                .into_iter()
                .map(|(session, mark)| Candidate {
                    session,
                    mark,
                    writer: Arc::clone(&writer),
                    relays: Arc::clone(&relays),
                }),
        );
    }
    if found.is_empty() {
        return;
    }
    lock(&hub.rejoins.inner).busy = true;
    let worker = Arc::clone(hub);
    if thread::Builder::new()
        .name("hub-rejoin".to_owned())
        .spawn(move || rejoin_pass(&worker, found))
        .is_err()
    {
        lock(&hub.rejoins.inner).busy = false;
    }
}

/// On the worker, with no lock held across IO. For each candidate:
/// advances its mark and stores it back for its epoch only. For a live one
/// whose socket accepts: `relay::attach` with the kept replay, exclusive.
/// A guard clears busy at the end (panic included). In tests it runs the
/// pause hook before each connect and signals pass-done at the end.
fn rejoin_pass(hub: &Arc<Hub>, candidates: Vec<Candidate>) {
    let _guard = BusyGuard {
        inner: &hub.rejoins.inner,
    };
    for candidate in candidates {
        let Candidate {
            session,
            mark,
            writer,
            relays,
        } = candidate;
        // No log at attach time records offset 0; the sweep finds the
        // log later at offset 0, and a session that exited cleanly
        // unlinked its socket, so only a bound name opens a log at all.
        if !hub.home.join("run").join(&session).exists() {
            continue;
        }
        let mark = if mark.log.is_none() {
            discovered(&hub.home, &session, mark.epoch).unwrap_or(mark)
        } else {
            mark
        };
        let advanced = advance(mark);
        let live = advanced.live;
        let candidate_epoch = advanced.epoch;
        {
            let mut held = lock(&relays);
            store_back(&mut held, &session, advanced);
        }
        if !live {
            continue;
        }
        #[cfg(test)]
        if let Some(before_connect) = lock(&hub.rejoins.before_connect).take() {
            before_connect();
        }
        // Fast rejection avoids connecting for a candidate already made
        // stale while it was off-lock. `attach_rejoin` repeats this check and
        // keeps the relays lock through replay and exclusive admission.
        if kept_for_candidate(&lock(&relays), &session, candidate_epoch).is_none() {
            continue;
        }
        let Ok(stream) = UnixStream::connect(hub.home.join("run").join(&session)) else {
            continue;
        };
        crate::relay::attach_rejoin(&session, stream, hub, &writer, &relays, candidate_epoch);
    }
    #[cfg(test)]
    if let Some(passed) = lock(&hub.rejoins.pass_done).as_ref() {
        passed.send(()).unwrap_or(());
    }
}

/// Clears the rejoin busy flag when the worker ends, a panic included.
struct BusyGuard<'a> {
    inner: &'a Mutex<Inner>,
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        lock(self.inner).busy = false;
    }
}

/// Seeks to `mark.read` and streams complete lines past it:
/// `fiber_started` makes it live; `fiber_exited` or `rewound` ends it;
/// read moves past the last complete line only, so a partial tail is read
/// again next time. Every consumed complete-line byte is read at most
/// once: bytes before the offset are never read. A file shorter than
/// `read` was replaced: resets read to 0 and live to false, then reads
/// the new file from its start.
fn advance(mark: Mark) -> Mark {
    let Some(log) = mark.log.clone() else {
        return mark;
    };
    let Ok(file) = std::fs::File::open(&log) else {
        return mark;
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let (mut read, mut live) = if len < mark.read {
        // The log was replaced under the same id: start over.
        (0, false)
    } else {
        (mark.read, mark.live)
    };
    use std::io::{BufRead, Seek, SeekFrom};
    let mut reader = std::io::BufReader::new(file);
    if reader.seek(SeekFrom::Start(read)).is_err() {
        return Mark {
            log: Some(log),
            read,
            live,
            epoch: mark.epoch,
        };
    }
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let Some((&last, line)) = buf.split_last() else {
                    break;
                };
                if last != b'\n' {
                    // A partial tail: read again next time.
                    break;
                }
                match kind_of(line) {
                    Some(kind) if kind == "fiber_started" => live = true,
                    Some(kind) if kind == "fiber_exited" || kind == "rewound" => live = false,
                    _ => {}
                }
                read += buf.len() as u64;
            }
        }
    }
    Mark {
        log: Some(log),
        read,
        live,
        epoch: mark.epoch,
    }
}

#[cfg(test)]
#[path = "rejoin_tests.rs"]
mod tests;
