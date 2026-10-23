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
use std::io::{BufReader, ErrorKind};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
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

/// How long a sessions stop waits for every session to confirm closed,
/// from the first zero-wait to the answer: inside the CLI's stop exchange
/// bound, with the answer still to come.
pub(crate) const SESSIONS_STOP_WITHIN: Duration = Duration::from_secs(10);

/// How long one session's socket has to accept, within
/// [`SESSIONS_STOP_WITHIN`]: the hub client's own bind wait, so a wedged
/// session holds one session, never the stop.
const CONNECT_DEADLINE: Duration = Duration::from_secs(5);

/// What one sessions stop found: either the listing failed, or the
/// stop ran over the listings. The lists exist only on the success
/// variant, so a failed listing can never read as an empty count.
pub(crate) enum Outcome {
    /// The `run/` listing failed: no session is named, stopped or
    /// running, and the stop answers `io_failed` instead of a count.
    ListFailed,
    /// The stop ran: the sessions whose sockets closed after their
    /// `close` with `now` was accepted, in id order, and every listed
    /// session neither confirmed nor skipped as refused, in id order.
    Listed {
        confirmed: Vec<SessionId>,
        unconfirmed: Vec<SessionId>,
    },
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
    let mut confirmed = BTreeSet::new();
    let mut gone = BTreeSet::new();
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
    fan_out(hub, &first, end, &mut confirmed, &mut gone);
    if hub.stop_requested() || hub.clock.now() >= end {
        return finish(first, Vec::new(), &confirmed, &gone);
    }
    zero_wait(hub, end);
    if hub.stop_requested() {
        return finish(first, Vec::new(), &confirmed, &gone);
    }
    let second = match list(hub) {
        Ok(second) => second,
        Err(_) => {
            return Outcome::ListFailed;
        }
    };
    let fresh: Vec<SessionId> = second
        .iter()
        .filter(|id| !confirmed.contains(&id.0))
        .cloned()
        .collect();
    fan_out(hub, &fresh, end, &mut confirmed, &mut gone);
    finish(first, second, &confirmed, &gone)
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
/// else is named.
fn probe(hub: &Hub) -> Outcome {
    let mut unconfirmed = Vec::new();
    let ids = match list(hub) {
        Ok(ids) => ids,
        Err(_) => {
            return Outcome::ListFailed;
        }
    };
    for id in ids {
        if connect_now(hub, &id) {
            unconfirmed.push(id);
        }
    }
    Outcome::Listed {
        confirmed: Vec::new(),
        unconfirmed,
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
fn fan_out(
    hub: &Arc<Hub>,
    ids: &[SessionId],
    end: Instant,
    confirmed: &mut BTreeSet<String>,
    gone: &mut BTreeSet<String>,
) {
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
                confirmed.insert(id);
            }
            SessionOutcome::Gone => {
                gone.insert(id);
            }
            SessionOutcome::Unconfirmed => {}
        }
    }
}

/// One fan-out's records: every worker records exactly once, so the
/// serving thread's wait ends when `done` reaches `total`.
#[derive(Default)]
struct Fanout {
    total: usize,
    done: usize,
    outcomes: Vec<(String, SessionOutcome)>,
}

impl Fanout {
    fn record(&mut self, id: String, outcome: SessionOutcome) {
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
    /// Its socket refused or was absent, or closed before its `close`
    /// was answered: already gone, skipped.
    Gone,
    /// Neither: still running, or gone past the bound.
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
    let stream = match connect_session(hub, id, end) {
        Connected::Stream(stream) => stream,
        Connected::Gone => return SessionOutcome::Gone,
        Connected::Unconfirmed => return SessionOutcome::Unconfirmed,
    };
    let Ok(read) = stream.try_clone() else {
        return SessionOutcome::Unconfirmed;
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
        Wrote::Gone => return SessionOutcome::Gone,
        Wrote::Unconfirmed => return SessionOutcome::Unconfirmed,
    }
    match read_ack(&mut read, &stream, hub, &subscribe_id, end) {
        Ack::Accepted => {}
        Ack::Rejected => return SessionOutcome::Unconfirmed,
        Ack::Gone => return SessionOutcome::Gone,
        Ack::Unconfirmed => return SessionOutcome::Unconfirmed,
    }
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

/// Merges both listings: confirmed in id order, with every listed session
/// neither confirmed nor skipped as refused unconfirmed in id order.
fn finish(
    first: Vec<SessionId>,
    second: Vec<SessionId>,
    confirmed: &BTreeSet<String>,
    gone: &BTreeSet<String>,
) -> Outcome {
    let mut seen = BTreeSet::new();
    let mut unconfirmed = Vec::new();
    for id in first.into_iter().chain(second) {
        if !confirmed.contains(&id.0) && !gone.contains(&id.0) && seen.insert(id.0.clone()) {
            unconfirmed.push(id);
        }
    }
    let confirmed = confirmed.iter().map(|id| SessionId(id.clone())).collect();
    Outcome::Listed {
        confirmed,
        unconfirmed,
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "sessions_stop_tests.rs"]
mod tests;
