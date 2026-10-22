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
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use contract::SessionId;
use contract::clock::Wake;
use serde_json::Value;

use crate::connection::Hub;
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

/// One real `poll` slice inside a clock wait: short enough that a fake
/// clock stays in charge of the bound, long enough that a live socket
/// wakes the wait early instead of spinning on it.
const POLL_SLICE: Duration = Duration::from_millis(10);

/// What one sessions stop found.
pub(crate) struct Outcome {
    /// The sessions whose sockets closed after their `close` with `now`
    /// was accepted, in id order.
    pub(crate) confirmed: Vec<SessionId>,
    /// Every listed session neither confirmed nor skipped as refused, in
    /// id order: still running, or gone past the bound.
    pub(crate) unconfirmed: Vec<SessionId>,
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
    let first = list(hub);
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
    let second = list(hub);
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
/// filter the feed scan applies before it connects.
fn list(hub: &Hub) -> Vec<SessionId> {
    let mut ids: Vec<SessionId> = fs::read_dir(hub.home.join("run"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| valid_session_id(name))
                .map(SessionId)
                .collect()
        })
        .unwrap_or_default();
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    ids
}

/// The no-wait probe for an abort before any listing ran: one
/// non-blocking connect attempt per entry, with no readiness wait and no
/// retry. Refused or absent is gone and skipped, as in the scan; anything
/// else is named.
fn probe(hub: &Hub) -> Outcome {
    let mut unconfirmed = Vec::new();
    for id in list(hub) {
        if connect_now(hub, &id) {
            unconfirmed.push(id);
        }
    }
    Outcome {
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
                if !wait_ready(stream.as_fd(), libc::POLLIN, hub, end) {
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
                if !wait_ready(fd.as_fd(), libc::POLLOUT, hub, until) {
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

/// What one non-blocking connect attempt found.
enum ConnectError {
    /// The kernel queued the connect: wait for readiness, then read the
    /// deferred error.
    Queued,
    /// Refused or absent: already gone.
    Gone,
    /// A refused backlog: retry on a fresh fd.
    Busy,
    /// Anything else: still running, or gone past the bound.
    Unconfirmed,
}

/// Starts one non-blocking connect: immediate success, a queued connect,
/// or the error answered once, so the backlog retry runs on a fresh fd.
// Exempt: AF_UNIX connects never return EINPROGRESS on Linux, so the
// queued arm never runs: no test can take the wait path.
#[cfg_attr(false, mutants::skip)]
fn start_connect(fd: BorrowedFd, socket: &std::path::Path) -> Result<(), ConnectError> {
    let Ok((addr, len)) = unix_addr(socket) else {
        return Err(ConnectError::Unconfirmed);
    };
    if raw_connect(fd.as_raw_fd(), &addr, len) == 0 {
        return Ok(());
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(code) if code == libc::EINPROGRESS => Err(ConnectError::Queued),
        Some(code) if code == libc::EAGAIN => Err(ConnectError::Busy),
        Some(code)
            if code == libc::ENOENT || code == libc::ECONNREFUSED || code == libc::ENOTSOCK =>
        {
            Err(ConnectError::Gone)
        }
        _ => Err(ConnectError::Unconfirmed),
    }
}

/// Reads one queued connect's deferred error: connected, refused, or
/// anything else.
fn check_connected(fd: BorrowedFd) -> Result<(), ConnectError> {
    match raw_socket_error(fd.as_raw_fd()) {
        0 => Ok(()),
        code if code == libc::ECONNREFUSED || code == libc::ENOENT => Err(ConnectError::Gone),
        _ => Err(ConnectError::Unconfirmed),
    }
}

/// Waits for `events` on `fd` until `until` on the injected clock: true
/// when the poll saw readiness. One [`POLL_SLICE`] at a time, so a fake
/// clock stays in charge of the bound while a live socket wakes the wait
/// early; every pass re-checks the stop, so an abort ends the wait.
fn wait_ready(fd: BorrowedFd, events: libc::c_short, hub: &Hub, until: Instant) -> bool {
    loop {
        if hub.stop_requested() || hub.clock.now() >= until {
            return false;
        }
        let mut ready = false;
        hub.clock.wait_until(Some(until), &mut |_bound| {
            ready = poll_ready(fd, events);
        });
        if ready {
            return true;
        }
    }
}

/// One readiness slice: true when the poll saw any event, error or hangup
/// included, as the hub client's own poll does.
fn poll_ready(fd: BorrowedFd, events: libc::c_short) -> bool {
    // POLL_SLICE is ten milliseconds: the cast cannot truncate.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "POLL_SLICE is ten milliseconds"
    )]
    let timeout = POLL_SLICE.as_millis() as libc::c_int;
    // The watched bits are disjoint, so `+` is their union, and an
    // arithmetic mutant changes the mask where `^` would not: raw
    // `c_short` has no union method, and no dependency buys one.
    raw_poll(fd.as_raw_fd(), events, timeout)
        & (libc::POLLIN + libc::POLLOUT + libc::POLLERR + libc::POLLHUP + libc::POLLNVAL)
            as libc::c_short
        != 0
}

/// What one bounded write found.
enum Wrote {
    /// The whole line went out.
    Done,
    /// The socket died: the session is gone.
    Gone,
    /// The bound passed or the stop landed first.
    Unconfirmed,
}

/// Writes one line, waiting for writability on the injected clock: the
/// lines are small, so one write usually takes them whole.
// Exempt: a nonempty write never returns 0 on a live socket, and the
// broken-pipe guard arm returns what its fallthrough returns: neither
// mutant changes any outcome.
#[cfg_attr(false, mutants::skip)]
fn write_line_bounded(stream: &UnixStream, line: &Value, hub: &Hub, end: Instant) -> Wrote {
    let Ok(mut bytes) = serde_json::to_vec(line) else {
        return Wrote::Unconfirmed;
    };
    bytes.push(b'\n');
    let mut out: &UnixStream = stream;
    let mut done = 0;
    loop {
        // `done` stays below `bytes.len()`: the loop returns Done once it reaches it.
        #[allow(clippy::indexing_slicing, reason = "done stays below bytes.len()")]
        let chunk = &bytes[done..];
        match out.write(chunk) {
            Ok(0) => return Wrote::Gone,
            Ok(n) => {
                done += n;
                if done >= bytes.len() {
                    return Wrote::Done;
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if !wait_ready(stream.as_fd(), libc::POLLOUT, hub, end) {
                    return Wrote::Unconfirmed;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::NotConnected
                ) =>
            {
                return Wrote::Gone;
            }
            Err(_) => return Wrote::Gone,
        }
    }
}

/// What one bounded acknowledgement read found.
enum Ack {
    /// The session accepted the command.
    Accepted,
    /// The session rejected the command: it is alive.
    Rejected,
    /// The socket closed or died first: the session is gone.
    Gone,
    /// The bound passed or the stop landed first.
    Unconfirmed,
}

/// Reads lines until the session's acknowledgement for `command_id`,
/// skipping every other line, on the injected clock. Incomplete bytes
/// stay in `pending` across readiness waits: a split line parses whole
/// once its tail arrives, instead of losing its head on `WouldBlock`.
// Exempt: serde_json tolerates the unstripped newline, and the exact
// 64 MiB boundary needs chunking control no test can force: neither
// mutant changes any acknowledgement.
#[cfg_attr(false, mutants::skip)]
fn read_ack(
    read: &mut BufReader<UnixStream>,
    stream: &UnixStream,
    hub: &Hub,
    command_id: &str,
    end: Instant,
) -> Ack {
    let mut pending: Vec<u8> = Vec::new();
    loop {
        if let Some(pos) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=pos).collect();
            let mut line = line;
            while line
                .last()
                .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
            {
                line.pop();
            }
            let ack: Value = match serde_json::from_slice(&line) {
                Ok(ack) => ack,
                Err(_) => continue,
            };
            let named = ack
                .get("payload")
                .and_then(|payload| payload.get("command_id"))
                .and_then(Value::as_str)
                == Some(command_id);
            if !named {
                continue;
            }
            match ack.get("kind").and_then(Value::as_str) {
                Some("command_accepted") => return Ack::Accepted,
                Some("command_rejected") => return Ack::Rejected,
                _ => continue,
            }
        }
        match read.fill_buf() {
            Ok([]) => {
                if pending.is_empty() {
                    return Ack::Gone;
                }
                // End of input with a final partial line: parse it
                // once, then the drained empty tail reports gone.
                let mut line = std::mem::take(&mut pending);
                while line.last().is_some_and(|byte| *byte == b'\r') {
                    line.pop();
                }
                let ack: Value = match serde_json::from_slice(&line) {
                    Ok(ack) => ack,
                    Err(_) => return Ack::Gone,
                };
                let named = ack
                    .get("payload")
                    .and_then(|payload| payload.get("command_id"))
                    .and_then(Value::as_str)
                    == Some(command_id);
                if !named {
                    return Ack::Gone;
                }
                match ack.get("kind").and_then(Value::as_str) {
                    Some("command_accepted") => return Ack::Accepted,
                    Some("command_rejected") => return Ack::Rejected,
                    _ => return Ack::Gone,
                }
            }
            Ok(buf) => {
                let chunk = buf.to_vec();
                if pending.len() + chunk.len() > support::framing::MAX_LINE + 1 {
                    return Ack::Gone;
                }
                let take = chunk.len();
                read.consume(take);
                pending.extend_from_slice(&chunk);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if !wait_ready(stream.as_fd(), libc::POLLIN, hub, end) {
                    return Ack::Unconfirmed;
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Ack::Gone,
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
    Outcome {
        confirmed,
        unconfirmed,
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Opens a non-blocking, close-on-exec stream socket for one bounded
/// connect: Apple has no `SOCK_NONBLOCK`, so blocking is cleared after
/// opening, as the hub client's own socket is.
#[allow(
    unsafe_code,
    reason = "the raw socket call below returns a new fd nobody else holds"
)]
// Exempt: both callers discard the error as Unconfirmed, and the only
// divergence (fd 0) needs closing process stdin: no test can observe
// the comparison.
#[cfg_attr(false, mutants::skip)]
fn open_socket() -> std::io::Result<OwnedFd> {
    // SAFETY: `socket` returns a new file descriptor owned by this call;
    // on error nothing is owned and the error returns.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // From here the fd is owned: every error closes it before returning.
    let owned = own_fd(fd);
    if set_nonblocking(owned.as_fd()) != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if set_cloexec(owned.as_fd()) != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(owned)
}

/// Takes ownership of a fd this module opened: nothing else holds it.
#[allow(
    unsafe_code,
    reason = "the fd comes from the raw socket call above, owned by this module"
)]
fn own_fd(fd: RawFd) -> OwnedFd {
    // SAFETY: `fd` is a new socket this module just opened and has not
    // shared; ownership moves into the returned value, which closes it.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// Marks the fd non-blocking: 0 on success.
#[allow(
    unsafe_code,
    reason = "fcntl takes only the owned fd with integer flags"
)]
fn set_nonblocking(fd: BorrowedFd) -> libc::c_int {
    // SAFETY: `fd` is a socket this module owns; `F_SETFL` with
    // `O_NONBLOCK` touches only that fd's flags.
    unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) }
}

/// Marks the fd close-on-exec: 0 on success.
#[allow(
    unsafe_code,
    reason = "fcntl takes only the owned fd with integer flags"
)]
fn set_cloexec(fd: BorrowedFd) -> libc::c_int {
    // SAFETY: `fd` is a socket this module owns; `F_SETFD` with
    // `FD_CLOEXEC` touches only that fd's flags.
    unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) }
}

/// Binds a Unix address for `socket`: the path must fit with its trailing
/// NUL, as a session socket's short `run/<id>` path always does.
#[allow(
    unsafe_code,
    reason = "zeroing a local address struct nobody else reaches"
)]
fn unix_addr(socket: &std::path::Path) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: the struct is a local never shared; zeroing it only writes
    // the local, and the NUL terminator is already there.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    // AF_UNIX fits in a sa_family_t.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "AF_UNIX fits in a sa_family_t"
    )]
    let family = libc::AF_UNIX as libc::sa_family_t;
    addr.sun_family = family;
    let bytes = socket.as_os_str().as_bytes();
    if bytes.len() >= addr.sun_path.len() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "the socket path is longer than its address holds",
        ));
    }
    for (slot, byte) in addr.sun_path.iter_mut().zip(bytes.iter()) {
        // The path bytes copy raw into sun_path.
        #[allow(clippy::cast_possible_wrap, reason = "the path bytes copy raw")]
        let raw = *byte as libc::c_char;
        *slot = raw;
    }
    // The length check above bounds the path, so the sum fits.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the length check above bounds the path"
    )]
    let len = (std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
    Ok((addr, len))
}

/// Starts the connect: 0 when it completed, `-1` with the error in
/// `errno` when it queued or failed.
#[allow(
    unsafe_code,
    reason = "connect reads only the caller-built address for its length"
)]
fn raw_connect(fd: RawFd, addr: &libc::sockaddr_un, len: libc::socklen_t) -> libc::c_int {
    // SAFETY: `addr` is a local the caller built for at least `len`
    // bytes; `connect` reads only those bytes and takes no ownership.
    unsafe {
        libc::connect(
            fd,
            addr as *const libc::sockaddr_un as *const libc::sockaddr,
            len,
        )
    }
}

/// Reads one queued connect's deferred error: 0 when it connected.
#[allow(
    unsafe_code,
    reason = "getsockopt writes only the local error slot for its length"
)]
// Exempt: when `getsockopt` itself fails this returns -1, and
// `check_connected` maps every nonzero code to `Gone` or `Unconfirmed`
// (`0 => Ok`, refused/missing => `Gone`, `_` => `Unconfirmed`), so -1 and
// the `1` mutant take the same arm: no test can separate them.
#[cfg_attr(false, mutants::skip)]
fn raw_socket_error(fd: RawFd) -> libc::c_int {
    // SAFETY: `err` is a local `c_int` and `len` names exactly it, so
    // `getsockopt` writes only the local; the call takes no ownership.
    let mut err: libc::c_int = 0;
    // A c_int's size fits in a socklen_t.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a c_int's size fits in a socklen_t"
    )]
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `err` is a local `c_int` and `len` names exactly it, so
    // `getsockopt` writes only the local; the call takes no ownership.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            &mut err as *mut libc::c_int as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 { -1 } else { err }
}

/// One readiness poll: the events the fd showed, or 0 on timeout or
/// error. The wait, not this slice, decides what an error means.
#[allow(
    unsafe_code,
    reason = "poll watches only the one local pollfd for one slice"
)]
fn raw_poll(fd: RawFd, events: libc::c_short, timeout_ms: libc::c_int) -> libc::c_short {
    // SAFETY: `watched` is a local naming one fd this module holds;
    // `poll` reads only it for one slice and takes no ownership.
    let mut watched = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    // SAFETY: `watched` is a local naming one fd this module holds;
    // `poll` reads only it for one slice and takes no ownership.
    let rc = unsafe { libc::poll(&mut watched, 1, timeout_ms) };
    if rc <= 0 { 0 } else { watched.revents }
}

#[cfg(test)]
#[path = "sessions_stop_tests.rs"]
mod tests;
