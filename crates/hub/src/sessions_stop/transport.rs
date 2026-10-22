//! The sessions-stop socket transport: one session socket opened,
//! connected, polled, read and written, with the error types the loop
//! in `super` matches on. Everything here takes what it needs as
//! arguments (a borrowed fd, `hub` for its clock and stop, the bound):
//! nothing reaches back into the parent's state. The socket, flag,
//! address, connect, error and poll calls are the safe `rustix`
//! wrappers the hub client dials with; the units the tests drive by
//! hand (`open_socket`, `start_connect`, `check_connected` and
//! `poll_ready`) stay on the same narrow surface.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{Errno, FdFlags, fcntl_setfd};
use rustix::net::{self, SocketAddrUnix, sockopt};
use serde_json::Value;

use crate::connection::Hub;

/// One real `poll` slice inside a clock wait: short enough that a fake
/// clock stays in charge of the bound, long enough that a live socket
/// wakes the wait early instead of spinning on it.
const POLL_SLICE: Duration = Duration::from_millis(10);

/// What one non-blocking connect attempt found.
pub(crate) enum ConnectError {
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
pub(crate) fn start_connect(fd: BorrowedFd, socket: &std::path::Path) -> Result<(), ConnectError> {
    let addr = match SocketAddrUnix::new(socket) {
        Ok(addr) => addr,
        Err(_) => return Err(ConnectError::Unconfirmed),
    };
    match net::connect(fd, &addr) {
        Ok(()) => Ok(()),
        Err(Errno::INPROGRESS) => Err(ConnectError::Queued),
        Err(Errno::AGAIN) => Err(ConnectError::Busy),
        Err(Errno::NOENT) | Err(Errno::CONNREFUSED) | Err(Errno::NOTSOCK) => {
            Err(ConnectError::Gone)
        }
        Err(_) => Err(ConnectError::Unconfirmed),
    }
}

/// Reads one queued connect's deferred error: connected, refused, or
/// anything else.
pub(crate) fn check_connected(fd: BorrowedFd) -> Result<(), ConnectError> {
    match sockopt::socket_error(fd) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(Errno::CONNREFUSED)) | Ok(Err(Errno::NOENT)) => Err(ConnectError::Gone),
        _ => Err(ConnectError::Unconfirmed),
    }
}

/// Waits for `events` on `fd` until `until` on the injected clock: true
/// when the poll saw readiness. One [`POLL_SLICE`] at a time, so a fake
/// clock stays in charge of the bound while a live socket wakes the wait
/// early; every pass re-checks the stop, so an abort ends the wait.
pub(crate) fn wait_ready(fd: BorrowedFd, events: PollFlags, hub: &Hub, until: Instant) -> bool {
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
/// included, as the hub client's own poll does: the watched bits join the
/// hangup and error bits, since a hangup or error is itself readiness.
pub(crate) fn poll_ready(fd: BorrowedFd, events: PollFlags) -> bool {
    // POLL_SLICE stays under two seconds: well below the signed range.
    let spec = Timespec {
        tv_sec: POLL_SLICE.as_secs().min(i64::MAX as u64).cast_signed(),
        tv_nsec: POLL_SLICE.subsec_nanos() as _,
    };
    let mut watched = [PollFd::new(
        &fd,
        events.union(PollFlags::HUP).union(PollFlags::ERR),
    )];
    // The park itself is the wait: a signal delivery or a woken fd both
    // end it, so the count is discarded.
    #[allow(
        clippy::let_underscore_must_use,
        reason = "a woken fd and an expired wait both end the park: the count is unused"
    )]
    let _ = poll(&mut watched, Some(&spec));
    watched
        .first()
        .map(PollFd::revents)
        .is_some_and(|revents| !revents.is_empty())
}

/// What one bounded write found.
pub(crate) enum Wrote {
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
pub(crate) fn write_line_bounded(
    stream: &UnixStream,
    line: &Value,
    hub: &Hub,
    end: Instant,
) -> Wrote {
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
                if !wait_ready(stream.as_fd(), PollFlags::OUT, hub, end) {
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
pub(crate) enum Ack {
    /// The session accepted the command.
    Accepted,
    /// The session rejected the command: it is alive.
    Rejected,
    /// The socket closed or died first: the session is gone.
    Gone,
    /// The bound passed or the stop landed first.
    Unconfirmed,
}

/// Parses one stripped line for `command_id`'s acknowledgement: the
/// acknowledgement, or `None` when the line carries no parse or none
/// for this command.
fn ack_of(line: &[u8], command_id: &str) -> Option<Ack> {
    let ack: Value = serde_json::from_slice(line).ok()?;
    let named = ack
        .get("payload")
        .and_then(|payload| payload.get("command_id"))
        .and_then(Value::as_str)
        == Some(command_id);
    if !named {
        return None;
    }
    match ack.get("kind").and_then(Value::as_str) {
        Some("command_accepted") => Some(Ack::Accepted),
        Some("command_rejected") => Some(Ack::Rejected),
        _ => None,
    }
}

/// Reads lines until the session's acknowledgement for `command_id`,
/// skipping every other line, on the injected clock. Every complete
/// buffered line drains before the socket is touched again: one write
/// can hold events plus the answer, and at end of input only a final
/// partial line remains for the single parse below. Incomplete bytes
/// stay in `pending` across readiness waits: a split line parses whole
/// once its tail arrives, instead of losing its head on `WouldBlock`.
// Exempt: serde_json tolerates the unstripped newline, and the exact
// 64 MiB boundary needs chunking control no test can force: neither
// mutant changes any acknowledgement.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn read_ack(
    read: &mut BufReader<UnixStream>,
    stream: &UnixStream,
    hub: &Hub,
    command_id: &str,
    end: Instant,
) -> Ack {
    let mut pending: Vec<u8> = Vec::new();
    loop {
        while let Some(pos) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=pos).collect();
            let mut line = line;
            while line
                .last()
                .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
            {
                line.pop();
            }
            if let Some(ack) = ack_of(&line, command_id) {
                return ack;
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
                match ack_of(&line, command_id) {
                    Some(ack) => return ack,
                    None => return Ack::Gone,
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
                if !wait_ready(stream.as_fd(), PollFlags::IN, hub, end) {
                    return Ack::Unconfirmed;
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Ack::Gone,
        }
    }
}

/// Opens a non-blocking, close-on-exec stream socket for one bounded
/// connect: Apple has no `SOCK_NONBLOCK`, so blocking is cleared after
/// opening, as the hub client's own socket is.
pub(crate) fn open_socket() -> std::io::Result<OwnedFd> {
    let fd = net::socket(net::AddressFamily::UNIX, net::SocketType::STREAM, None)?;
    fcntl_setfl(&fd, fcntl_getfl(&fd)?.union(OFlags::NONBLOCK))?;
    fcntl_setfd(&fd, FdFlags::CLOEXEC)?;
    Ok(fd)
}
