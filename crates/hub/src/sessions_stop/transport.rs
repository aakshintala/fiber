//! The sessions-stop socket transport: one session socket opened,
//! connected, polled, read and written, with the error types the loop
//! in `super` matches on. Everything here takes what it needs as
//! arguments (a borrowed fd, `hub` for its clock and stop, the bound):
//! nothing reaches back into the parent's state. The loop uses
//! `open_socket`, `start_connect`, `check_connected`, `wait_ready`,
//! `write_line_bounded` and `read_ack`; the seams the unit tests drive
//! by hand (`fcntl_flag`, `unix_addr`, `raw_poll`) stay on the same
//! surface.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

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
pub(crate) fn check_connected(fd: BorrowedFd) -> Result<(), ConnectError> {
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
pub(crate) fn wait_ready(fd: BorrowedFd, events: libc::c_short, hub: &Hub, until: Instant) -> bool {
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
// Exempt: the second `+`'s `-` mutant clears only `POLLHUP`, and the
// revents survey shows `POLLHUP` never surfaces bare on this platform
// (a hangup reads `0x1`, a reset carries `0x18` = ERR|HUP), so the
// mutant is behavior-preserving and no test can bite. The first `+`'s
// twin clears `POLLERR` and dies on the bare-`0x8` case, kept as the
// regression test.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn poll_ready(fd: BorrowedFd, events: libc::c_short) -> bool {
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
/// skipping every other line, on the injected clock. Incomplete bytes
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
        if let Some(pos) = pending.iter().position(|byte| *byte == b'\n') {
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
                if !wait_ready(stream.as_fd(), libc::POLLIN, hub, end) {
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
#[allow(
    unsafe_code,
    reason = "the raw socket call below returns a new fd nobody else holds"
)]
// Exempt: both callers discard the error as Unconfirmed, and the only
// divergence (fd 0) needs closing process stdin: no test can observe
// the comparison.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn open_socket() -> std::io::Result<OwnedFd> {
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

/// Applies one `fcntl` flag command to the raw descriptor number: the
/// contract is value only, so a closed number fails `EBADF` with no
/// borrow involved. Production calls through the safe `BorrowedFd`
/// wrappers below, which lend a live owned fd.
#[allow(
    unsafe_code,
    reason = "the seam takes the raw number by value: no borrow, no ownership"
)]
pub(crate) unsafe fn fcntl_flag(fd: RawFd, command: libc::c_int, flag: libc::c_int) -> libc::c_int {
    // SAFETY: `fcntl` reads no memory and takes no ownership: a number
    // naming nothing fails `EBADF`, and the wrappers pass a live fd.
    unsafe { libc::fcntl(fd, command, flag) }
}

/// Marks the fd non-blocking: 0 on success.
// Exempt: one-line plumbing over the probed `fcntl_flag` seam, whose
// closed-number probe pins the failure below; no test can reach the
// wrapper with a closed fd without the unsound borrow the seam removes.
#[cfg_attr(false, mutants::skip)]
#[allow(
    unsafe_code,
    reason = "fcntl takes only the borrowed fd's number with integer flags"
)]
fn set_nonblocking(fd: BorrowedFd) -> libc::c_int {
    // SAFETY: `fd` borrows a live owned fd for the call below, so the
    // number names it through the flag write.
    unsafe { fcntl_flag(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) }
}

/// Marks the fd close-on-exec: 0 on success.
// Exempt: one-line plumbing over the probed `fcntl_flag` seam, whose
// closed-number probe pins the failure below; no test can reach the
// wrapper with a closed fd without the unsound borrow the seam removes.
#[cfg_attr(false, mutants::skip)]
#[allow(
    unsafe_code,
    reason = "fcntl takes only the borrowed fd's number with integer flags"
)]
fn set_cloexec(fd: BorrowedFd) -> libc::c_int {
    // SAFETY: `fd` borrows a live owned fd for the call below, so the
    // number names it through the flag write.
    unsafe { fcntl_flag(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) }
}

/// Binds a Unix address for `socket`: the path must fit with its trailing
/// NUL, as a session socket's short `run/<id>` path always does.
#[allow(
    unsafe_code,
    reason = "zeroing a local address struct nobody else reaches"
)]
pub(crate) fn unix_addr(
    socket: &std::path::Path,
) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
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
pub(crate) fn raw_poll(fd: RawFd, events: libc::c_short, timeout_ms: libc::c_int) -> libc::c_short {
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
