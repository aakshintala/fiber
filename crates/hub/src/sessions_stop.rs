//! Shutting down every running session before the hub exits: the
//! sessions-first half of a `stop` with `sessions` true
//! (`docs/invocation.md`, "What the hub speaks").
//!
//! The loop lists `run/`, connects to each session socket through a bounded
//! non-blocking connect, subscribes `summary` first on every connection,
//! then sends `close` with `now` and waits for each socket to close, every
//! session in parallel under [`SESSIONS_STOP_WITHIN`]. A second zero-wait
//! and one rescan catch the starters the first scan missed. Every wait the
//! serving thread makes ends on [`Hub::stop_requested`], so a signal or a
//! bare `stop` mid-loop answers the partial stop at once instead of
//! waiting the loop out.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead as _, BufReader, ErrorKind};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use contract::SessionId;
use contract::clock::Wake;
use rustix::event::PollFlags;
use transport::{
    Ack, ConnectError, Wrote, check_connected, open_socket, read_ack, start_connect, wait_ready,
    write_line_bounded,
};

use crate::connection::Hub;

pub(crate) mod transport;
use crate::relay::valid_session_id;
use crate::tick::Wait;
use support::lock;

/// How long a sessions stop waits for every session to confirm closed,
/// from the first zero-wait to the answer: inside the CLI's stop exchange
/// bound, with the answer still to come.
pub(crate) const SESSIONS_STOP_WITHIN: Duration = Duration::from_secs(10);

/// How long one session's socket has to accept, within
/// [`SESSIONS_STOP_WITHIN`]: the hub client's own bind wait, so a wedged
/// session holds one session, never the stop. The `stop_keepers`
/// command shares it inside its own bound.
const CONNECT_DEADLINE: Duration = Duration::from_secs(5);

/// How long a `stop_keepers` command waits for every named keeper to
/// confirm stopped: the serving thread's fan-out wait, with the connect
/// bound inside it and the answer still to come.
pub(crate) const KEEPERS_STOP_WITHIN: Duration = Duration::from_secs(10);

/// What one sessions stop found: either the listing failed, or the
/// stop ran over the listings. The lists exist only on the success
/// variant, so a failed listing can never read as an empty count.
pub(crate) enum Outcome {
    /// The `run/` listing failed: no session is named, stopped or
    /// running, and the stop answers `io_failed` instead of a count.
    ListFailed,
    /// The stop ran: the sessions whose sockets closed after their
    /// `close` with `now` was accepted, in id order; the keepers whose
    /// sockets closed after the frozen stop, in id order; every listed
    /// session neither confirmed nor skipped as refused, in id order; and
    /// every keeper-orbit id neither confirmed nor skipped, in id order.
    Listed {
        confirmed: Vec<SessionId>,
        keepers: Vec<SessionId>,
        unconfirmed: Vec<SessionId>,
        unconfirmed_keepers: Vec<SessionId>,
    },
}

/// The serving thread's running tally: every set holds ids in id order,
/// and every worker records exactly once.
#[derive(Default)]
struct Tally {
    /// Sessions whose sockets closed after their `close` with `now`.
    confirmed: BTreeSet<String>,
    /// Keepers whose sockets closed after the frozen stop.
    keepers: BTreeSet<String>,
    /// Sessions whose sockets refused or were absent, or closed before
    /// their `close` was answered.
    gone: BTreeSet<String>,
}

/// Whether the rescan still has to stop `id`: the first scan neither
/// confirmed nor keeper-confirmed its stop. Exempt: re-stopping an
/// already-closed socket reads gone and leaves the outcome unchanged,
/// so only redundant connects differ, which no consumer sees; no test
/// can stage a visible difference.
#[cfg_attr(false, mutants::skip)]
fn rescan_still_stops(tally: &Tally, id: &SessionId) -> bool {
    !tally.confirmed.contains(&id.0) && !tally.keepers.contains(&id.0)
}

/// Shuts down every running session the hub knows of in `run/`, each
/// through its own shutdown (`close` with `now`), bounded by
/// [`SESSIONS_STOP_WITHIN`]: zero-wait on `inflight_binds`, scan, zero-wait
/// and rescan, then the confirmed-closed count with the ids unconfirmed at
/// the return. A session whose socket refuses or is absent is already gone
/// and is skipped, never counted. Only worker-observed closes count: a
/// socket that vanishes without one is gone, not confirmed.
pub(crate) fn run(hub: &Arc<Hub>) -> Outcome {
    let end = hub.clock.now() + SESSIONS_STOP_WITHIN;
    let mut tally = Tally::default();
    zero_wait(hub, end);
    if hub.stop_requested() {
        return probe(hub);
    }
    let first = match list(hub) {
        Ok(first) => first,
        Err(_) => {
            return Outcome::ListFailed;
        }
    };
    #[cfg(test)]
    if let Some(hook) = lock(&hub.on_scan_listed).take() {
        hook();
    }
    if hub.stop_requested() {
        return probe(hub);
    }
    fan_out(hub, &first, end, &mut tally);
    if hub.stop_requested() || hub.clock.now() >= end {
        return finish(hub, first, Vec::new(), &tally);
    }
    zero_wait(hub, end);
    if hub.stop_requested() {
        return finish(hub, first, Vec::new(), &tally);
    }
    let second = match list(hub) {
        Ok(second) => second,
        Err(_) => {
            return Outcome::ListFailed;
        }
    };
    let fresh: Vec<SessionId> = second
        .iter()
        .filter(|id| rescan_still_stops(&tally, id))
        .cloned()
        .collect();
    fan_out(hub, &fresh, end, &mut tally);
    finish(hub, first, second, &tally)
}

/// What stopping the named keepers found: every confirmed stop and
/// every keeper neither confirmed nor skipped as gone, each in id
/// order. Live sessions are reported apart: they were left running,
/// never stopped.
pub(crate) struct KeepersOutcome {
    /// The keepers whose sockets closed after the frozen stop.
    pub(crate) confirmed: Vec<SessionId>,
    /// The named ids that answered the verify: live sessions.
    pub(crate) live: Vec<SessionId>,
    /// The named keepers neither confirmed nor skipped, in id order.
    pub(crate) unconfirmed: Vec<SessionId>,
}

/// Stops exactly the named keepers through their stop, bounded by
/// [`KEEPERS_STOP_WITHIN`]: one subscribe-first verify per id, then the
/// frozen stop for keeper-or-gone sockets through the same transport
/// the sessions stop uses. An answered subscribe is a live session:
/// left running and reported, never stopped. A refused socket is gone
/// and skipped. A keeper still silent past the bound is unconfirmed.
/// The hub keeps running after answering: nothing is stored, and no
/// stop is requested.
pub(crate) fn stop_keepers(hub: &Arc<Hub>, ids: &[SessionId]) -> KeepersOutcome {
    let end = hub.clock.now() + KEEPERS_STOP_WITHIN;
    let shared = Arc::new(Mutex::new(Fanout {
        total: ids.len(),
        done: 0,
        outcomes: Vec::new(),
    }));
    for id in ids {
        let hub = Arc::clone(hub);
        let id = id.clone();
        let name = id.0.clone();
        let fallback = id.0.clone();
        let recorded = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("hub-stop-keeper".to_owned())
            .spawn(move || {
                let outcome = stop_one_verified(&hub, &id, end);
                lock(&recorded).record(name, outcome);
                hub.tick.wake();
            });
        if spawned.is_err() {
            lock(&shared).record(fallback, KeeperOutcome::Unconfirmed);
        }
    }
    hub.tick.wait_for(&*hub.clock, &mut |now| {
        if hub.stop_requested() {
            return Wait::Done;
        }
        if lock(&shared).settled() {
            return Wait::Done;
        }
        if now >= end {
            return Wait::Done;
        }
        #[cfg(test)]
        if let Some(hook) = lock(&hub.on_fanout_wait).take() {
            hook();
        }
        Wait::Until(Some(end))
    });
    let mut confirmed = Vec::new();
    let mut live = Vec::new();
    let mut unconfirmed = Vec::new();
    let mut recorded = BTreeSet::new();
    for (id, outcome) in lock(&shared).outcomes.drain(..) {
        recorded.insert(id.clone());
        match outcome {
            KeeperOutcome::Confirmed => confirmed.push(SessionId(id)),
            KeeperOutcome::Live => live.push(SessionId(id)),
            KeeperOutcome::Unconfirmed => unconfirmed.push(SessionId(id)),
            KeeperOutcome::Gone => {}
        }
    }
    // Every named id with no record by the bound reads Unconfirmed:
    // past the bound, unreported means unconfirmed. A refused socket
    // records Gone through the worker path, so gone ids stay skipped.
    // A worker record arriving after this drain lands in this run's
    // map only, which drops with it: never merged here or later.
    for id in ids {
        if !recorded.contains(&id.0) {
            unconfirmed.push(id.clone());
        }
    }
    confirmed.sort_by(|a, b| a.0.cmp(&b.0));
    live.sort_by(|a, b| a.0.cmp(&b.0));
    unconfirmed.sort_by(|a, b| a.0.cmp(&b.0));
    KeepersOutcome {
        confirmed,
        live,
        unconfirmed,
    }
}

/// What one named keeper's verify-and-stop found.
enum KeeperOutcome {
    /// Its socket closed after the frozen stop was written.
    Confirmed,
    /// Its subscribe was answered: a live session, left running.
    Live,
    /// Its socket refused or was absent: already gone, skipped.
    Gone,
    /// Neither: still silent past the bound, or gone past it.
    Unconfirmed,
}

/// What one bounded subscribe-first verify found.
enum Verified {
    /// Answered its subscribe: a live session, with the connection
    /// the close leg continues on.
    Live(UnixStream, BufReader<UnixStream>),
    /// EOF without an answer: a keeper holding the crashed socket, or
    /// a session gone mid-handshake. The stop leg tells them apart.
    KeeperOrGone,
    /// Refused or absent: already gone.
    Gone,
    /// Anything else short of an answer: still running, or gone past
    /// the bound.
    Unconfirmed,
}

/// Verifies one id with the sessions stop's opening exchange: bounded
/// connect, `summary` subscribe first on every connection, then the
/// read of its acknowledgement. (A refused or absent socket never
/// reaches the read: it is gone at connect.) Every wait shares the
/// loop deadline and ends on the stop.
fn verify_one(hub: &Hub, id: &SessionId, end: Instant) -> Verified {
    let stream = match connect_session(hub, id, end) {
        Connected::Stream(stream) => stream,
        Connected::Gone => return Verified::Gone,
        Connected::Unconfirmed => return Verified::Unconfirmed,
    };
    let Ok(read) = stream.try_clone() else {
        return Verified::Unconfirmed;
    };
    let mut read = BufReader::new(read);
    let subscribe_id = support::mint("c_hub_stop_");
    let subscribe = serde_json::json!({
        "id": subscribe_id,
        "command": "subscribe",
        "args": { "level": "summary" },
    });
    match write_line_bounded(&stream, &subscribe, hub, end) {
        Wrote::Done => {}
        Wrote::Gone => return Verified::Gone,
        Wrote::Unconfirmed => return Verified::Unconfirmed,
    }
    match read_ack(&mut read, &stream, hub, &subscribe_id, end) {
        Ack::Accepted => Verified::Live(stream, read),
        Ack::Rejected => Verified::Unconfirmed,
        // EOF without an answer is a keeper holding the crashed
        // socket, or a session gone mid-handshake: the stop leg tells
        // them apart, as in the sessions stop.
        Ack::Gone => Verified::KeeperOrGone,
        Ack::Unconfirmed => Verified::Unconfirmed,
    }
}

/// Verifies one named keeper, then stops it: the subscribe-first
/// verify, then the frozen stop for keeper-or-gone sockets through the
/// same transport the sessions stop uses.
fn stop_one_verified(hub: &Hub, id: &SessionId, end: Instant) -> KeeperOutcome {
    match verify_one(hub, id, end) {
        // A live session answers its subscribe: left running, never
        // stopped.
        Verified::Live(..) => return KeeperOutcome::Live,
        Verified::KeeperOrGone => {}
        Verified::Gone => return KeeperOutcome::Gone,
        Verified::Unconfirmed => return KeeperOutcome::Unconfirmed,
    }
    match stop_keeper(hub, id, end) {
        SessionOutcome::KeeperConfirmed => KeeperOutcome::Confirmed,
        SessionOutcome::Gone => KeeperOutcome::Gone,
        SessionOutcome::Confirmed | SessionOutcome::Unconfirmed => KeeperOutcome::Unconfirmed,
    }
}

/// Waits for no bind to be in flight, on the loop deadline: a starter
/// admitted before the store binds or fails first, so no admitted session
/// escapes the scan. Ends at once on the stop.
fn zero_wait(hub: &Arc<Hub>, end: Instant) {
    hub.tick.wait_for(&*hub.clock, &mut |now| {
        if hub.stop_requested() {
            return Wait::Done;
        }
        if hub.inflight_binds.load(Ordering::SeqCst) == 0 {
            return Wait::Done;
        }
        if now >= end {
            return Wait::Done;
        }
        #[cfg(test)]
        if let Some(hook) = lock(&hub.on_inflight_wait).take() {
            hook();
        }
        Wait::Until(Some(end))
    });
}

/// The `run/` entries that name a session, in id order: the same read and
/// filter the feed scan applies before it connects. A missing `run/` is
/// no entries, as for a home that never started a session. Any other
/// open or entry failure is `Err`: the stop answers `io_failed` instead
/// of accepting a partial list as the whole.
fn list(hub: &Hub) -> std::io::Result<Vec<SessionId>> {
    let dir = match fs::read_dir(hub.home.join("run")) {
        Ok(dir) => dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };
    let mut ids = Vec::new();
    for entry in dir {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if valid_session_id(&name) {
            ids.push(SessionId(name));
        }
    }
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(ids)
}

/// The no-wait probe for an abort before any listing ran: one
/// non-blocking connect attempt per entry, with no readiness wait and no
/// retry. Refused or absent is gone and skipped, as in the scan; anything
/// else is named, keeper-held ids with the keeper part.
fn probe(hub: &Hub) -> Outcome {
    let mut connectable = Vec::new();
    let ids = match list(hub) {
        Ok(ids) => ids,
        Err(_) => {
            return Outcome::ListFailed;
        }
    };
    for id in ids {
        if connect_now(hub, &id) {
            connectable.push(id);
        }
    }
    let (unconfirmed, unconfirmed_keepers) = split_unresolved(hub, connectable);
    Outcome::Listed {
        confirmed: Vec::new(),
        keepers: Vec::new(),
        unconfirmed,
        unconfirmed_keepers,
    }
}

/// One non-blocking connect attempt: false when refused or absent, true
/// for anything else, with no wait and no retry.
fn connect_now(hub: &Hub, id: &SessionId) -> bool {
    let socket = hub.home.join("run").join(&id.0);
    let Ok(fd) = open_socket() else {
        return true;
    };
    let named = match start_connect(fd.as_fd(), &socket) {
        Ok(()) => true,
        Err(ConnectError::Gone) => false,
        Err(_) => true,
    };
    drop(fd);
    named
}

/// Stops every listed session in parallel, recording each outcome: the
/// serving thread waits only for the records, the stop or the deadline,
/// never for a worker.
fn fan_out(hub: &Arc<Hub>, ids: &[SessionId], end: Instant, tally: &mut Tally) {
    let shared = Arc::new(Mutex::new(Fanout {
        total: ids.len(),
        done: 0,
        outcomes: Vec::new(),
    }));
    for id in ids {
        let hub = Arc::clone(hub);
        let id = id.clone();
        let name = id.0.clone();
        let fallback = id.0.clone();
        let recorded = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("hub-stop-session".to_owned())
            .spawn(move || {
                let outcome = stop_one(&hub, &id, end);
                lock(&recorded).record(name, outcome);
                hub.tick.wake();
            });
        if spawned.is_err() {
            lock(&shared).record(fallback, SessionOutcome::Unconfirmed);
        }
    }
    hub.tick.wait_for(&*hub.clock, &mut |now| {
        if hub.stop_requested() {
            return Wait::Done;
        }
        if lock(&shared).settled() {
            return Wait::Done;
        }
        if now >= end {
            return Wait::Done;
        }
        #[cfg(test)]
        if let Some(hook) = lock(&hub.on_fanout_wait).take() {
            hook();
        }
        Wait::Until(Some(end))
    });
    for (id, outcome) in lock(&shared).outcomes.drain(..) {
        match outcome {
            SessionOutcome::Confirmed => {
                tally.confirmed.insert(id);
            }
            SessionOutcome::KeeperConfirmed => {
                tally.keepers.insert(id);
            }
            SessionOutcome::Gone => {
                tally.gone.insert(id);
            }
            SessionOutcome::Unconfirmed => {}
        }
    }
}

/// One fan-out's records, over either stop's outcome: every worker
/// records exactly once, so the serving thread's wait ends when
/// `done` reaches `total`.
#[derive(Default)]
struct Fanout<O> {
    total: usize,
    done: usize,
    outcomes: Vec<(String, O)>,
}

impl<O> Fanout<O> {
    fn record(&mut self, id: String, outcome: O) {
        self.done += 1;
        self.outcomes.push((id, outcome));
    }

    fn settled(&self) -> bool {
        self.done >= self.total
    }
}

/// What one session's stop found.
enum SessionOutcome {
    /// Its socket closed after its `close` with `now` was accepted.
    Confirmed,
    /// Its socket closed after the frozen stop was written: its keeper
    /// stopped.
    KeeperConfirmed,
    /// Its socket refused or was absent, or closed before its `close`
    /// was answered: already gone, skipped.
    Gone,
    /// Neither: still running, or gone past the bound. The keeper leg's
    /// timeouts and answers land here too: the finish split names a
    /// still-held keeper with the keeper part.
    Unconfirmed,
}

/// Stops one session: bounded connect, `summary` subscribe first, `close`
/// with `now`, then the socket's close. Every wait shares the loop
/// deadline and ends on the stop.
// Exempt: after the close acknowledgement both queues are drained, so a
// reset surfaces as clean EOF, never a fatal error in the final read:
// the WouldBlock guard's only other input is unreachable.
#[cfg_attr(false, mutants::skip)]
fn stop_one(hub: &Hub, id: &SessionId, end: Instant) -> SessionOutcome {
    let (stream, mut read) = match verify_one(hub, id, end) {
        Verified::Live(stream, read) => (stream, read),
        // EOF without an answer is a keeper holding the crashed
        // socket, or a session gone mid-handshake: the keeper leg
        // tells them apart.
        Verified::KeeperOrGone => return stop_keeper(hub, id, end),
        Verified::Gone => return SessionOutcome::Gone,
        Verified::Unconfirmed => return SessionOutcome::Unconfirmed,
    };
    let close_id = support::mint("c_hub_stop_");
    let close = serde_json::json!({
        "id": close_id,
        "command": "close",
        "args": { "now": true },
    });
    match write_line_bounded(&stream, &close, hub, end) {
        Wrote::Done => {}
        Wrote::Gone => return SessionOutcome::Gone,
        Wrote::Unconfirmed => return SessionOutcome::Unconfirmed,
    }
    match read_ack(&mut read, &stream, hub, &close_id, end) {
        Ack::Accepted => {}
        Ack::Rejected => return SessionOutcome::Unconfirmed,
        Ack::Gone => return SessionOutcome::Gone,
        Ack::Unconfirmed => return SessionOutcome::Unconfirmed,
    }
    loop {
        match support::framing::read_line(&mut read, support::framing::MAX_LINE) {
            Ok(Some(_)) => {}
            Ok(None) => return SessionOutcome::Confirmed,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if !wait_ready(stream.as_fd(), PollFlags::IN, hub, end) {
                    return SessionOutcome::Unconfirmed;
                }
            }
            // A reset socket is closed, however uncleanly: the session
            // is gone after its close was accepted.
            Err(_) => return SessionOutcome::Confirmed,
        }
    }
}

/// What one keeper read decides: the stop's outcome, or another read
/// after a readiness wait.
enum KeeperRead {
    /// The read settled the stop.
    Done(SessionOutcome),
    /// The socket may yet answer: wait for readiness, then read again.
    Again,
}

/// Decides one keeper read, with no I/O: a keeper never writes, so any
/// line is a live session left running, a clean EOF or a closed socket
/// confirms the stop, and anything else is unconfirmed at once.
/// `WouldBlock` alone waits for another read.
fn decide_keeper_read(read: std::io::Result<Option<Vec<u8>>>) -> KeeperRead {
    match read {
        Ok(Some(_)) => KeeperRead::Done(SessionOutcome::Unconfirmed),
        Ok(None) => KeeperRead::Done(SessionOutcome::KeeperConfirmed),
        Err(error) if error.kind() == ErrorKind::WouldBlock => KeeperRead::Again,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionReset | ErrorKind::BrokenPipe | ErrorKind::ConnectionAborted
            ) =>
        {
            KeeperRead::Done(SessionOutcome::KeeperConfirmed)
        }
        Err(_) => KeeperRead::Done(SessionOutcome::Unconfirmed),
    }
}

/// Stops one keeper: a fresh connection, the frozen stop line, then the
/// socket's close. Only a closed socket after the write confirms, clean
/// EOF or a reset, broken pipe or aborted connection alike: a refused or
/// failed write is a session gone mid-handshake, anything the holder
/// writes back is a live session, any other read error is unconfirmed at
/// once, and the bound leaves the rest unconfirmed. Every wait shares the
/// loop deadline and ends on the stop.
fn stop_keeper(hub: &Hub, id: &SessionId, end: Instant) -> SessionOutcome {
    let stream = match connect_session(hub, id, end) {
        Connected::Stream(stream) => stream,
        Connected::Gone => return SessionOutcome::Gone,
        Connected::Unconfirmed => return SessionOutcome::Unconfirmed,
    };
    let Ok(read) = stream.try_clone() else {
        return SessionOutcome::Unconfirmed;
    };
    let mut read = BufReader::new(read);
    // The exact frozen bytes a newer Fiber writes an older keeper
    // (`docs/invocation.md`, "Commands and flags").
    let stop = serde_json::json!({"shutdown": true});
    match write_line_bounded(&stream, &stop, hub, end) {
        Wrote::Done => {}
        Wrote::Gone => return SessionOutcome::Gone,
        Wrote::Unconfirmed => return SessionOutcome::Unconfirmed,
    }
    // Bytes stay in `pending` across readiness waits; see `keeper_line`.
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match decide_keeper_read(keeper_line(&mut read, &mut pending)) {
            KeeperRead::Done(outcome) => return outcome,
            KeeperRead::Again => {
                if !wait_ready(stream.as_fd(), PollFlags::IN, hub, end) {
                    return SessionOutcome::Unconfirmed;
                }
            }
        }
    }
}

/// One line for the keeper leg, accumulated across readiness waits:
/// bytes the socket yields stay in `pending`, so a mid-drain
/// `WouldBlock` never loses the head the way a fresh `read_line`
/// would. Reports what `framing::read_line` would whenever the bytes
/// arrive, however many waits that takes: a complete or partial-final
/// line as `Some`, a clean EOF as `None`, past-the-cap as
/// `InvalidData`.
// Exempt: which chunk trips the cap and whether a tail is partial
// needs chunking control no test can force, as in `read_ack`; the
// `WouldBlock` guard in the loop above stays mutant-covered by the
// overlong tests.
#[cfg_attr(false, mutants::skip)]
fn keeper_line(
    read: &mut BufReader<UnixStream>,
    pending: &mut Vec<u8>,
) -> std::io::Result<Option<Vec<u8>>> {
    loop {
        match read.fill_buf() {
            Ok([]) => {
                if pending.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(std::mem::take(pending)));
            }
            Ok(buf) => {
                if pending.len() + buf.len() > support::framing::MAX_LINE {
                    return Err(std::io::Error::new(
                        ErrorKind::InvalidData,
                        "a line is longer than its cap",
                    ));
                }
                if let Some(end) = buf.iter().position(|byte| *byte == b'\n') {
                    // `end` is a position inside `buf`: the slice cannot
                    // miss.
                    #[allow(clippy::indexing_slicing, reason = "position() stays within buf")]
                    let head = &buf[..end];
                    pending.extend_from_slice(head);
                    read.consume(end + 1);
                    return Ok(Some(std::mem::take(pending)));
                }
                let chunk = buf.to_vec();
                read.consume(chunk.len());
                pending.extend_from_slice(&chunk);
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Splits ids the stop left unresolved by who holds their session log:
/// keeper-held ids are reported with the keeper part, the rest as
/// sessions, each in the listing's order. Runs on the serving thread
/// after the fan-out, so a keeper that stopped but whose worker has not
/// recorded yet reads released — the same lateness a session close
/// already has.
fn split_unresolved(
    hub: &Hub,
    ids: impl IntoIterator<Item = SessionId>,
) -> (Vec<SessionId>, Vec<SessionId>) {
    let mut sessions = Vec::new();
    let mut keepers = Vec::new();
    for id in ids {
        if log::find_session(&hub.home, &id)
            .is_some_and(|(_, dir)| crate::resume::keeper_holds(&dir))
        {
            keepers.push(id);
        } else {
            sessions.push(id);
        }
    }
    (sessions, keepers)
}

/// What one bounded connect found.
enum Connected {
    /// The session's socket, non-blocking.
    Stream(UnixStream),
    /// Refused or absent: already gone.
    Gone,
    /// Anything else short of a socket: still running, or gone past the
    /// bound.
    Unconfirmed,
}

/// Connects to one session socket, bounded by [`CONNECT_DEADLINE`] within
/// the loop deadline: a refused backlog retries on a fresh fd, as the hub
/// client's own attempt does.
// Exempt: AF_UNIX connects complete synchronously into the backlog on
// Linux, so the queued branch never runs: EINPROGRESS never occurs.
#[cfg_attr(false, mutants::skip)]
fn connect_session(hub: &Hub, id: &SessionId, end: Instant) -> Connected {
    let socket = hub.home.join("run").join(&id.0);
    let until = hub
        .clock
        .now()
        .checked_add(CONNECT_DEADLINE)
        .unwrap_or(end)
        .min(end);
    loop {
        if hub.stop_requested() || hub.clock.now() >= until {
            return Connected::Unconfirmed;
        }
        let Ok(fd) = open_socket() else {
            return Connected::Unconfirmed;
        };
        match start_connect(fd.as_fd(), &socket) {
            Ok(()) => return Connected::Stream(UnixStream::from(fd)),
            Err(ConnectError::Queued) => {
                if !wait_ready(fd.as_fd(), PollFlags::OUT, hub, until) {
                    return Connected::Unconfirmed;
                }
                match check_connected(fd.as_fd()) {
                    Ok(()) => return Connected::Stream(UnixStream::from(fd)),
                    Err(ConnectError::Gone) => return Connected::Gone,
                    Err(_) => return Connected::Unconfirmed,
                }
            }
            Err(ConnectError::Busy) => {}
            Err(ConnectError::Gone) => return Connected::Gone,
            Err(ConnectError::Unconfirmed) => return Connected::Unconfirmed,
        }
    }
}

/// Merges both listings: confirmed sessions and keepers in id order,
/// with every listed session neither confirmed nor skipped as refused
/// split into sessions and still-held keepers, each in the listings'
/// order. Keeper-orbit ids a worker resolved stay out of both either
/// way.
fn finish(hub: &Hub, first: Vec<SessionId>, second: Vec<SessionId>, tally: &Tally) -> Outcome {
    let mut seen = BTreeSet::new();
    let mut unresolved = Vec::new();
    for id in first.into_iter().chain(second) {
        if !tally.confirmed.contains(&id.0)
            && !tally.keepers.contains(&id.0)
            && !tally.gone.contains(&id.0)
            && seen.insert(id.0.clone())
        {
            unresolved.push(id);
        }
    }
    let (unconfirmed, unconfirmed_keepers) = split_unresolved(hub, unresolved);
    let confirmed = tally
        .confirmed
        .iter()
        .map(|id| SessionId(id.clone()))
        .collect();
    let keepers = tally
        .keepers
        .iter()
        .map(|id| SessionId(id.clone()))
        .collect();
    Outcome::Listed {
        confirmed,
        keepers,
        unconfirmed,
        unconfirmed_keepers,
    }
}

#[cfg(test)]
#[path = "sessions_stop_tests.rs"]
mod tests;
