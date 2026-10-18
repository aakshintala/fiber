//! The hub client (`docs/invocation.md`, "The hub"): connecting to
//! `run/hub`, starting one when none runs.
//!
//! A client starts the hub when none is running, then retries connecting
//! on the injected clock. The first line is read byte by byte, without
//! buffered over-read, and must be `hub_hello`: EOF or a reset before it
//! is the idle-exit race, and the whole connect repeats past each race
//! until the race bound, pausing one poll between attempts. The line must
//! arrive within one deadline on the injected clock, fixed when the socket
//! accepts, so a hub that accepts and never speaks, or trickles its line,
//! cannot hang the client; the returned stream has no read timeout.
//!
//! Every wait parks on the injected clock through [`Clock::wait_until`],
//! so tests step fake time through each wait, while the socket itself
//! never blocks: connects run non-blocking and reads poll for readiness,
//! so one real bound never stalls another clock. [`probe_until`] shares
//! the engine without ever starting a hub, reporting absence, failure,
//! and post-accept results separately for `fiber doctor`.

mod address;
mod listed;
pub use address::{Address, NAME_RULE, check_name};
pub use listed::{Target, connect_target, connect_unpaired, load_target, target};

use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::{HubLine, SCHEMA_VERSION};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{Errno, FdFlags, fcntl_setfd};
use rustix::net::{self, SocketAddrUnix, sockopt};

/// How long `connect` retries the hub's socket on the injected clock.
pub const CONNECT_DEADLINE: Duration = Duration::from_secs(5);

/// How often `connect` retries on the injected clock.
const CONNECT_POLL: Duration = Duration::from_millis(10);

/// The longest one real poll waits: fake waits slice here, then re-check
/// the fake deadline, so every wait stays interruptible by the injected
/// clock while a real hub still wakes the poll early.
const FAKE_WAIT_CAP: Duration = Duration::from_millis(10);

/// Which hub the client talks to, with its opening line.
///
/// The stream is ready for driver commands; `hello` is the `hub_hello` it
/// spoke first.
pub type Hub = (UnixStream, HubLine);

/// Connects to the hub's socket in `home`, starting one through `start`
/// when none runs. `start` runs once at most per attempt, so a retry
/// after the idle-exit race may start a new hub. Returns the stream and the
/// `hub_hello` it spoke first, which must arrive within [`CONNECT_DEADLINE`].
pub fn connect(
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
) -> io::Result<Hub> {
    connect_with(
        home,
        Some(start),
        clock,
        Due::Within(CONNECT_DEADLINE),
        &mut || {},
        &mut || {},
        &mut || {},
    )
    .map_err(io_from_poll)
}

/// [`connect`], with `hub_hello` due by `deadline` on `clock`: the same
/// absolute deadline bounds every attempt and the handshake, so a peer
/// that closes just before it leaves no fresh deadline for a retry.
/// Between races the client pauses one poll, and `start` may run again
/// once the socket is gone. Past it the connect fails `TimedOut`.
/// `before_read` runs after each handshake read's deadline check, just
/// before the read.
pub fn connect_until(
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
    deadline: Instant,
    before_read: &mut dyn FnMut(),
) -> io::Result<Hub> {
    let total = deadline.saturating_duration_since(clock.now());
    connect_with(
        home,
        Some(start),
        clock,
        Due::At { deadline, total },
        before_read,
        &mut || {},
        &mut || {},
    )
    .map_err(io_from_poll)
}
/// [`connect`], with `hub_hello` due within `hello_within` of the socket
/// accepting, on `clock`. Between races the client pauses one poll, and
/// `start` may run again once the socket is gone. Past the race bound the
/// connect fails `UnexpectedEof`, or `TimedOut` when `hello_within`
/// overflowed the bound and fell back to [`CONNECT_DEADLINE`]
pub fn connect_within(
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
    hello_within: Duration,
) -> io::Result<Hub> {
    connect_with(
        home,
        Some(start),
        clock,
        Due::Within(hello_within),
        &mut || {},
        &mut || {},
        &mut || {},
    )
    .map_err(io_from_poll)
}

/// Folds a [`Poll`] into the connectors' error: post-accept and pre-accept
/// failures surface as their error, while a race past the bound is the
/// idle exit, which cannot reach here.
fn io_from_poll(poll: Poll) -> io::Error {
    match poll {
        Poll::Race => unreachable_race(),
        Poll::Failed(error) | Poll::Accepted(error) | Poll::Absent(error) => error,
    }
}

/// The idle exit as an error: EOF or a reset before `hub_hello`, past the
/// race bound. The race loop exhausts into this; it never escapes it.
fn unreachable_race() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the hub closed the connection before `hub_hello`",
    )
}

/// Carries which deadline an attempt and its handshake read use:
/// [`connect_until`]'s one absolute deadline for both, or
/// [`connect_within`]'s per-attempt bind wait with a hello deadline from
/// accept.
enum Due {
    /// One absolute `deadline` bounds both attempts and the handshake;
    /// `total` names it in the hello message.
    At { deadline: Instant, total: Duration },
    /// Each attempt's bind wait is [`CONNECT_DEADLINE`] from its start;
    /// the hello is due this long after the socket accepts.
    Within(Duration),
}

impl Due {
    /// The bind wait's deadline: the shared one for `At`, one
    /// [`CONNECT_DEADLINE`] from this attempt's start for `Within`. Read
    /// once per attempt, so a retry under [`connect_until`] gets only what
    /// remains.
    fn bind_deadline(&self, now: Instant) -> Instant {
        match self {
            Due::At { deadline, .. } => *deadline,
            Due::Within(_) => now + CONNECT_DEADLINE,
        }
    }

    /// The handshake read's deadline: the shared one for `At`, the accept
    /// plus the hello wait for `Within`, or the attempt's bind deadline
    /// when that overflows, so no `Duration` panics and no wait is
    /// unbounded.
    fn hello_deadline(&self, accepted_at: Instant, bind: Instant) -> Option<Instant> {
        match self {
            Due::At { deadline, .. } => Some(*deadline),
            Due::Within(within) => accepted_at.checked_add(*within).or(Some(bind)),
        }
    }

    /// The race bound: a race retries only when the next attempt would
    /// start before it. The shared deadline for `At`, so every race,
    /// bind wait and handshake read ends by it; the first attempt's
    /// start plus [`CONNECT_DEADLINE`] for `Within`, so the retries end
    /// together while each attempt still waits its own bind deadline.
    /// `Within`'s hello wait does not move it: a 10 s hello wait still
    /// stops racing after one connect deadline, as
    /// `a_hub_that_never_speaks_fails_eof_at_the_race_bound` pins.
    fn race_bound(&self, first_start: Instant) -> Instant {
        match self {
            Due::At { deadline, .. } => *deadline,
            Due::Within(_) => first_start
                .checked_add(CONNECT_DEADLINE)
                .unwrap_or(first_start),
        }
    }

    /// Whether `Within`'s hello wait overflowed the race bound, so
    /// exhaustion past it is a hello timeout, not a closed connection:
    /// the peer kept dropping a handshake with no hello deadline of its
    /// own, bounded only by the fallback.
    fn overflowed(&self, first_start: Instant) -> bool {
        match self {
            Due::At { .. } => false,
            Due::Within(within) => first_start.checked_add(*within).is_none(),
        }
    }

    /// The duration the hello timeout message names.
    fn named(&self) -> Duration {
        match self {
            Due::At { total, .. } => *total,
            Due::Within(within) => *within,
        }
    }
}

/// The skeleton [`connect_until`] and [`connect_within`] share, and
/// [`probe_with`] uses: each [`attempt`] runs until it speaks `hub_hello`,
/// and the idle-exit race pauses one [`CONNECT_POLL`] and retries the
/// whole connect with a fresh `started`, while the retry would start
/// before the race bound. `start` runs at most once per attempt; `None`
/// never starts and reports an absent socket as [`Poll::Absent`]. Past the
/// bound the result is `UnexpectedEof` with no further sleep.
/// `accepted` runs when a connect succeeds, before any hello deadline
/// check; `before_wait` runs after each wait's deadline check, just
/// before [`Clock::wait_until`].
fn connect_with(
    home: &Path,
    mut start: Option<&mut dyn FnMut() -> io::Result<()>>,
    clock: &dyn Clock,
    due: Due,
    before_read: &mut dyn FnMut(),
    accepted: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Result<Hub, Poll> {
    let first_start = clock.now();
    let bound = due.race_bound(first_start);
    let overflowed = due.overflowed(first_start);
    let socket = home.join("run").join("hub");
    let mut started = false;
    loop {
        match attempt(
            home,
            &mut started,
            start
                .as_mut()
                .map(|start| start as &mut dyn FnMut() -> io::Result<()>),
            clock,
            &due,
            before_read,
            accepted,
            before_wait,
        ) {
            Ok(hub) => return Ok(hub),
            Err(Poll::Race) => {}
            Err(other) => return Err(other),
        }
        // EOF or a reset before `hub_hello` is the idle-exit race: the
        // hub exited before the handshake, so the connect retries past
        // each race until the bound, then may start a new hub once the
        // socket is gone.
        if clock.now().checked_add(CONNECT_POLL).unwrap_or(bound) >= bound {
            break;
        }
        started = false;
        clock.sleep(CONNECT_POLL);
    }
    if overflowed {
        return Err(Poll::Accepted(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "the hub at {} accepted but did not say hub_hello in {} s",
                socket.display(),
                CONNECT_DEADLINE.as_secs_f64()
            ),
        )));
    }
    Err(Poll::Failed(unreachable_race()))
}

/// What one handshake byte-read means.
enum HelloRead {
    /// One byte towards the line.
    Byte,
    /// End of stream or a reset before the line: the idle-exit race.
    Race,
    /// The deadline passed while reading.
    TimedOut,
    /// Any other read failure.
    Failed(io::Error),
}

/// Classifies one handshake byte-read, so tests inject each outcome.
/// A reset is the same race: a connection queued on a listener that
/// closes unaccepted reads reset, not EOF, on some platforms.
fn classify_hello_read(read: io::Result<usize>) -> HelloRead {
    match read {
        Ok(0) => HelloRead::Race,
        Ok(_) => HelloRead::Byte,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            HelloRead::TimedOut
        }
        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => HelloRead::Race,
        Err(error) => HelloRead::Failed(error),
    }
}

/// What one connection attempt means: the fd, its hello, and its stream.
/// A [`Poll::Race`] keeps the race loop running; anything else hands the
/// outcome back, with post-accept failures as [`Poll::Accepted`] and an
/// absent socket as [`Poll::Absent`].
enum Poll {
    /// EOF or a reset before `hub_hello`: the whole connect retries while
    /// the retry would start before the race bound.
    Race,
    /// No retry: the error.
    Failed(io::Error),
    /// The socket accepted, then the handshake failed: no retry, and the
    /// error.
    Accepted(io::Error),
    /// No hub runs and none may start: no retry, and the error.
    Absent(io::Error),
}

/// What one bounded probe finds: no socket, a pre-connect failure, or a
/// connection with its handshake result. Post-accept failures ride
/// `Connected`'s error, so `fiber doctor` prints a running hub with a
/// broken handshake instead of an unknown one.
pub enum Probe {
    /// No hub runs: nothing listened and none may start.
    Absent,
    /// The connect failed before any accept.
    Failed(io::Error),
    /// The socket accepted: the stream with its `hub_hello`, or the
    /// handshake's error.
    Connected(Result<Hub, io::Error>),
}

/// [`probe_with`] with no hooks: the one absolute `deadline` bounds every
/// attempt and the handshake, and no hub ever starts.
pub fn probe_until(home: &Path, clock: &dyn Clock, deadline: Instant) -> Probe {
    probe_with(home, clock, deadline, &mut || {}, &mut || {})
}

/// Probes the hub's socket in `home` without ever starting one, through
/// the shared race loop: one absolute `deadline` bounds every attempt and
/// the handshake. `accepted` runs when a connect succeeds, before any
/// hello deadline check; `before_wait` runs after each wait's deadline
/// check, just before [`Clock::wait_until`].
pub fn probe_with(
    home: &Path,
    clock: &dyn Clock,
    deadline: Instant,
    accepted: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Probe {
    let total = deadline.saturating_duration_since(clock.now());
    match connect_with(
        home,
        None,
        clock,
        Due::At { deadline, total },
        &mut || {},
        accepted,
        before_wait,
    ) {
        Ok(hub) => Probe::Connected(Ok(hub)),
        Err(Poll::Race) => Probe::Failed(unreachable_race()),
        Err(Poll::Failed(error)) => Probe::Failed(error),
        Err(Poll::Accepted(error)) => Probe::Connected(Err(error)),
        Err(Poll::Absent(_)) => Probe::Absent,
    }
}

/// What a connect error answers: connected through the deferred error,
/// or the error itself with its socket, for the usual answer.
enum Deferred {
    /// The queued connect completed after all.
    Connected(Hub),
    /// No queued connect: answer the error as usual.
    Direct(OwnedFd, rustix::io::Errno),
}

/// Answers a connect the kernel queued instead of completing: waits for
/// readiness, then reads the deferred error — connected or failed. Absent
/// and busy come back untouched as [`Deferred::Direct`], for the one
/// answer in [`attempt`], so every connect error is handled once and the
/// bind wait runs a single time. No test forces the kernel to queue a
/// Unix-socket connect — the backlog tests connect synchronously on every
/// platform — so this waits out its mutants; [`is_in_progress`] is pinned
/// by unit tests instead.
#[cfg_attr(false, mutants::skip)]
#[allow(
    clippy::too_many_arguments,
    reason = "one deferred answer keeps the race loop flat: socket, error, clock, bounds and three hooks"
)]
fn finish_deferred(
    fd: OwnedFd,
    errno: rustix::io::Errno,
    socket: &Path,
    clock: &dyn Clock,
    due: &Due,
    bind: Instant,
    bind_timed_out: &dyn Fn() -> Poll,
    before_read: &mut dyn FnMut(),
    accepted: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Result<Deferred, Poll> {
    if !is_in_progress(&errno) {
        return Ok(Deferred::Direct(fd, errno));
    }
    if !wait_ready(fd.as_fd(), PollFlags::OUT, clock, bind, before_wait) {
        return Err(bind_timed_out());
    }
    match sockopt::socket_error(&fd) {
        Ok(Ok(())) => finish_connect(
            fd,
            socket,
            clock,
            due,
            bind,
            before_read,
            accepted,
            before_wait,
        )
        .map(Deferred::Connected),
        Ok(Err(errno)) => Ok(Deferred::Direct(fd, errno)),
        Err(errno) => Err(Poll::Failed(io::Error::from(errno))),
    }
}

/// Polls the hub's socket until it accepts and speaks `hub_hello`. The
/// bind wait ends at `due`'s deadline: the shared one for `At`, one
/// [`CONNECT_DEADLINE`] from this attempt's start for `Within`. The
/// socket never blocks: an in-flight connect waits for readiness, a full
/// backlog retries on a fresh socket, and absence starts one through
/// `start` or reports [`Poll::Absent`]. `accepted` runs when a connect
/// succeeds, before any hello deadline check.
#[allow(
    clippy::too_many_arguments,
    reason = "one bundled attempt keeps the race loop flat: target, starter, clock, bound and three hooks"
)]
fn attempt(
    home: &Path,
    started: &mut bool,
    mut start: Option<&mut dyn FnMut() -> io::Result<()>>,
    clock: &dyn Clock,
    due: &Due,
    before_read: &mut dyn FnMut(),
    accepted: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Result<Hub, Poll> {
    let socket = home.join("run").join("hub");
    let bind = due.bind_deadline(clock.now());
    let bind_named = match due {
        Due::At { total, .. } => *total,
        Due::Within(_) => CONNECT_DEADLINE,
    };
    let bind_timed_out = || {
        Poll::Failed(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "the hub did not bind {} in {} s",
                socket.display(),
                bind_named.as_secs_f64()
            ),
        ))
    };
    loop {
        let fd = nonblocking_socket().map_err(Poll::Failed)?;
        let addr = SocketAddrUnix::new(&socket)
            .map_err(io::Error::from)
            .map_err(Poll::Failed)?;
        match net::connect(&fd, &addr) {
            Ok(()) => {
                return finish_connect(
                    fd,
                    &socket,
                    clock,
                    due,
                    bind,
                    before_read,
                    accepted,
                    before_wait,
                );
            }
            Err(errno) => {
                match finish_deferred(
                    fd,
                    errno,
                    &socket,
                    clock,
                    due,
                    bind,
                    &bind_timed_out,
                    before_read,
                    accepted,
                    before_wait,
                )? {
                    Deferred::Connected(hub) => return Ok(hub),
                    Deferred::Direct(fd, errno) => {
                        let error = io::Error::from(errno);
                        // No hub runs when the socket is absent or nobody
                        // listens yet: the client starts one and keeps
                        // polling. Any other error fails as is.
                        if matches!(
                            error.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                        ) {
                            // The path can still be a hub socket when it is
                            // missing, or a socket file one left behind when
                            // it exited. Any other filesystem object never
                            // listened: on Linux its connect refuses exactly
                            // like an absent hub, while macOS reports a
                            // non-socket error, so the caller fails instead
                            // of starting or missing a hub.
                            let socket_or_missing = match std::fs::metadata(&socket) {
                                Err(_) => true,
                                Ok(meta) => meta.file_type().is_socket(),
                            };
                            if !socket_or_missing {
                                return Err(Poll::Failed(error));
                            }
                            wait_for_start(
                                start
                                    .as_mut()
                                    .map(|start| start as &mut dyn FnMut() -> io::Result<()>),
                                started,
                                clock,
                                bind,
                                &bind_timed_out,
                                error,
                            )?;
                        } else if is_busy(&errno) {
                            // A full backlog on Linux: the connect itself retries
                            // within the bind deadline, never starting a hub.
                            if clock.now() >= bind {
                                return Err(bind_timed_out());
                            }
                            before_wait();
                            clock.wait_until(Some(bind), &mut |bound| {
                                poll_once(fd.as_fd(), PollFlags::OUT, bound);
                            });
                        } else {
                            return Err(Poll::Failed(error));
                        }
                    }
                }
            }
        }
    }
}

/// Runs `start` once and pauses one [`CONNECT_POLL`] of the bind wait
/// when `error` says no hub runs: `Ok(())` retries the connect. With no
/// starter the absence reports [`Poll::Absent`] at once, never waiting.
fn wait_for_start(
    start: Option<&mut dyn FnMut() -> io::Result<()>>,
    started: &mut bool,
    clock: &dyn Clock,
    bind: Instant,
    bind_timed_out: &dyn Fn() -> Poll,
    error: io::Error,
) -> Result<(), Poll> {
    let Some(start) = start else {
        return Err(Poll::Absent(error));
    };
    if !*started {
        start().map_err(Poll::Failed)?;
        *started = true;
    }
    if clock.now() >= bind {
        return Err(bind_timed_out());
    }
    clock.sleep(CONNECT_POLL);
    Ok(())
}

/// A non-blocking, close-on-exec stream socket: Apple has no
/// `SOCK_NONBLOCK`, so blocking is cleared after opening.
pub(crate) fn nonblocking_socket() -> io::Result<OwnedFd> {
    let fd = net::socket(net::AddressFamily::UNIX, net::SocketType::STREAM, None)?;
    fcntl_setfl(&fd, fcntl_getfl(&fd)?.union(OFlags::NONBLOCK))?;
    fcntl_setfd(&fd, FdFlags::CLOEXEC)?;
    Ok(fd)
}

/// Waits for `events` on `fd` until `deadline`, parking through
/// [`Clock::wait_until`] after each deadline check: `true` when the poll
/// saw readiness, `false` past the deadline. `before_wait` runs after
/// each check, just before the park.
fn wait_ready(
    fd: BorrowedFd,
    events: PollFlags,
    clock: &dyn Clock,
    deadline: Instant,
    before_wait: &mut dyn FnMut(),
) -> bool {
    loop {
        if clock.now() >= deadline {
            return false;
        }
        before_wait();
        let mut ready = false;
        clock.wait_until(Some(deadline), &mut |bound| {
            ready = poll_once(fd, events, bound);
        });
        if ready {
            return true;
        }
    }
}

/// One readiness check: polls `fd` for `events` within the shorter of the
/// clock's bound and [`FAKE_WAIT_CAP`], so a fake clock's wait stays
/// interruptible while a real hub still wakes the poll early.
fn poll_once(fd: BorrowedFd, events: PollFlags, bound: Option<Duration>) -> bool {
    let mut fds = [PollFd::new(
        &fd,
        events.union(PollFlags::HUP).union(PollFlags::ERR),
    )];
    wait_poll(&mut fds, bound);
    fds.first()
        .map(PollFd::revents)
        .is_some_and(|events| !events.is_empty())
}

/// Polls `fds` for at most one [`FAKE_WAIT_CAP`] slice of `bound`: the
/// count is discarded, since readiness, not how many fds woke, decides.
fn wait_poll(fds: &mut [PollFd<'_>], bound: Option<Duration>) {
    let slice = bound
        .map(|bound| bound.min(FAKE_WAIT_CAP))
        .unwrap_or(FAKE_WAIT_CAP);
    let spec = Timespec {
        tv_sec: slice.as_secs().min(i64::MAX as u64).cast_signed(),
        tv_nsec: slice.subsec_nanos() as _,
    };
    // The park itself is the wait: a signal delivery or a woken fd both
    // end it, so the count is discarded.
    #[allow(
        clippy::let_underscore_must_use,
        reason = "a woken fd and an expired wait both end the park: the count is unused"
    )]
    let _ = poll(fds, Some(&spec));
}

/// `true` when `error` is `EINPROGRESS`: a non-blocking connect still
/// in flight.
fn is_in_progress(error: &Errno) -> bool {
    *error == Errno::INPROGRESS
}

/// `true` when `error` is `EAGAIN`: a connect refused by a full backlog,
/// which retries within the bind deadline without starting a hub.
fn is_busy(error: &Errno) -> bool {
    *error == Errno::AGAIN
}

/// The connected `fd`'s hello: records the acceptance, reads one
/// `hub_hello` line by [`due`]'s deadline, then hands back a blocking
/// stream with no read timeout. A hello failure is [`Poll::Accepted`].
#[allow(
    clippy::too_many_arguments,
    reason = "one bundled handshake keeps the accept path flat: socket, clock, bound and three hooks"
)]
fn finish_connect(
    fd: OwnedFd,
    socket: &Path,
    clock: &dyn Clock,
    due: &Due,
    bind: Instant,
    before_read: &mut dyn FnMut(),
    accepted: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Result<Hub, Poll> {
    accepted();
    let hello = due.hello_deadline(clock.now(), bind);
    let stream = UnixStream::from(fd);
    match poll_hello(
        stream,
        socket,
        hello,
        due.named(),
        clock,
        before_read,
        before_wait,
    ) {
        Ok((stream, line)) => {
            stream.set_nonblocking(false).map_err(Poll::Accepted)?;
            set_read_timeout(&stream, None).map_err(Poll::Accepted)?;
            Ok((stream, line))
        }
        Err(Poll::Race) => Err(Poll::Race),
        Err(Poll::Failed(error)) | Err(Poll::Accepted(error)) => Err(Poll::Accepted(error)),
        Err(Poll::Absent(error)) => Err(Poll::Accepted(error)),
    }
}

/// Reads the first line byte by byte, without buffered over-read. The
/// stream never blocks: each `WouldBlock` waits for readability through
/// [`wait_ready`], then the deadline is re-checked, so a trickling hub
/// cannot renew it. The line is due by `deadline`, fixed when the socket
/// accepted; `named` names it in the timeout message. `before_read` runs
/// after each deadline check, just before the read.
#[allow(
    clippy::too_many_arguments,
    reason = "one bundled handshake keeps the read path flat: socket, deadline, clock and two hooks"
)]
fn poll_hello(
    mut stream: UnixStream,
    socket: &Path,
    deadline: Option<Instant>,
    named: Duration,
    clock: &dyn Clock,
    before_read: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Result<Hub, Poll> {
    let timed_out = || {
        Poll::Failed(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "the hub at {} accepted but did not say hub_hello in {} s",
                socket.display(),
                named.as_secs_f64()
            ),
        ))
    };
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if let Some(deadline) = deadline
            && !deadline
                .checked_duration_since(clock.now())
                // At the deadline or past it no further read fits.
                .is_some_and(|left| !left.is_zero())
        {
            return Err(timed_out());
        }
        before_read();
        match stream.read(&mut byte) {
            Ok(0) => return Err(Poll::Race),
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
            Err(error) => {
                // Not a wait: the read itself failed, so its kind decides.
                // A `WouldBlock` match guard here leaves the always-wait
                // mutant no hello test can reach, while this form's flipped
                // comparison fails the silent-hub deadline tests.
                if error.kind() != io::ErrorKind::WouldBlock {
                    return Err(match classify_hello_read(Err(error)) {
                        HelloRead::Race => Poll::Race,
                        HelloRead::TimedOut => timed_out(),
                        HelloRead::Failed(error) => Poll::Failed(error),
                        // `classify_hello_read` never reports a byte for an
                        // error: not `hub_hello` first.
                        HelloRead::Byte => Poll::Failed(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "the hub did not speak `hub_hello` first",
                        )),
                    });
                }
                // Not yet, not too late: readability waits, then the
                // deadline is re-checked above.
                let expired = match deadline {
                    Some(deadline) => {
                        !wait_ready(stream.as_fd(), PollFlags::IN, clock, deadline, before_wait)
                    }
                    None => {
                        clock.wait_until(None, &mut |bound| {
                            poll_once(stream.as_fd(), PollFlags::IN, bound);
                        });
                        false
                    }
                };
                if expired {
                    return Err(timed_out());
                }
            }
        }
    }
    check_line(&mut buf).map(|hello| (stream, hello))
}

/// Requires the handshake line's `hub_hello` on this build's
/// `schema_version`: a carriage return ends the line too.
fn check_line(buf: &mut Vec<u8>) -> Result<HubLine, Poll> {
    // A `\r\n` ending speaks `hub_hello` too.
    while buf.last() == Some(&b'\r') {
        buf.pop();
    }
    let hello: HubLine = serde_json::from_slice(buf).map_err(|_| {
        Poll::Failed(io::Error::new(
            io::ErrorKind::InvalidData,
            "the hub did not speak `hub_hello` first",
        ))
    })?;
    if hello.kind != "hub_hello" {
        return Err(Poll::Failed(io::Error::new(
            io::ErrorKind::InvalidData,
            "the hub did not speak `hub_hello` first",
        )));
    }
    if hello.schema_version != SCHEMA_VERSION {
        return Err(Poll::Failed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the hub runs schema version {}, this Fiber runs schema version {SCHEMA_VERSION}; \
                 update Fiber or restart the hub, then reconnect",
                hello.schema_version,
            ),
        )));
    }
    Ok(hello)
}

/// Reads the first line byte by byte, without buffered over-read, and
/// requires `hub_hello` on this build's `schema_version`. The line is due
/// by `deadline` on `clock`, fixed when the socket accepted, or with no
/// timeout when the accept plus the hello wait overflowed. `before_read`
/// runs after each deadline check, just before the read. The stream comes
/// back blocking with no read timeout once the line is read.
fn read_hello(
    stream: UnixStream,
    socket: &Path,
    deadline: Option<Instant>,
    named: Duration,
    clock: &dyn Clock,
    before_read: &mut dyn FnMut(),
) -> Result<Hub, Poll> {
    stream.set_nonblocking(true).map_err(Poll::Failed)?;
    let (stream, hello) = poll_hello(
        stream,
        socket,
        deadline,
        named,
        clock,
        before_read,
        &mut || {},
    )?;
    stream.set_nonblocking(false).map_err(Poll::Failed)?;
    set_read_timeout(&stream, None).map_err(Poll::Failed)?;
    Ok((stream, hello))
}

/// Sets `stream`'s read timeout, ignoring the closed-peer refusal:
/// macOS refuses the option once the peer has closed (`InvalidInput`),
/// while its buffered bytes stay readable and a read of the closed socket
/// does not block.
fn set_read_timeout(stream: &UnixStream, timeout: Option<Duration>) -> io::Result<()> {
    match stream.set_read_timeout(timeout) {
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        other => other,
    }
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
