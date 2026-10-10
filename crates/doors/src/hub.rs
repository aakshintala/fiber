//! The hub client (`docs/invocation.md`, "The hub"): connecting to
//! `run/hub`, starting one when none runs.
//!
//! A client starts the hub when none is running, then retries connecting
//! on the injected clock. The first line is read byte by byte, without
//! buffered over-read, and must be `hub_hello`: EOF before it is the
//! idle-exit race, and the whole connect is retried once. The line must
//! arrive within one deadline on the injected clock, fixed when the socket
//! accepts, so a hub that accepts and never speaks, or trickles its line,
//! cannot hang the client; the returned stream has no read timeout.

use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::{HubLine, SCHEMA_VERSION};

/// How long `connect` retries the hub's socket on the injected clock.
pub const CONNECT_DEADLINE: Duration = Duration::from_secs(5);

/// How often `connect` retries on the injected clock.
const CONNECT_POLL: Duration = Duration::from_millis(10);

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
    connect_with(home, start, clock, Due::Within(CONNECT_DEADLINE), &mut || {})
}

/// [`connect`], with `hub_hello` due by `deadline` on `clock`: the same
/// absolute deadline bounds both connection attempts and the handshake,
/// so a peer that closes just before it leaves no fresh deadline for the
/// retry. Between the attempts the client pauses one poll, so the exiting
/// hub's listener is gone, and `start` may run again. Past it the connect
/// fails `TimedOut`. `before_read` runs after each handshake read's
/// deadline check, just before the read.
pub fn connect_until(
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
    deadline: Instant,
    before_read: &mut dyn FnMut(),
) -> io::Result<Hub> {
    let total = deadline.saturating_duration_since(clock.now());
    connect_with(home, start, clock, Due::At { deadline, total }, before_read)
}
/// [`connect`], with `hub_hello` due within `hello_within` of the socket
/// accepting, on `clock`. Between the attempts the client pauses one poll,
/// so the exiting hub's listener is gone, and `start` may run again. Past
/// it the connect fails `TimedOut`.
pub fn connect_within(
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
    hello_within: Duration,
) -> io::Result<Hub> {
    connect_with(home, start, clock, Due::Within(hello_within), &mut || {})
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
    /// plus the hello wait for `Within`, or none when that overflows.
    fn hello_deadline(&self, accepted_at: Instant) -> Option<Instant> {
        match self {
            Due::At { deadline, .. } => Some(*deadline),
            Due::Within(within) => accepted_at.checked_add(*within),
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

/// The two-attempt skeleton [`connect_until`] and [`connect_within`] share:
/// one [`attempt`], then exactly one [`CONNECT_POLL`] pause and a second
/// [`attempt`] with a fresh `started` when the first saw the idle-exit
/// race. `start` runs at most once per attempt. After the second race the
/// result is `UnexpectedEof` with no further sleep.
fn connect_with(
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
    due: Due,
    before_read: &mut dyn FnMut(),
) -> io::Result<Hub> {
    let mut started = false;
    match attempt(home, &mut started, start, clock, &due, before_read) {
        Ok(hub) => return Ok(hub),
        Err(Poll::Race) => {}
        Err(Poll::Failed(error)) => return Err(error),
    }
    // EOF before `hub_hello` is the idle-exit race: the hub exited before
    // the handshake, so the retry pauses one poll for its listener to go
    // away, then may start a new hub when the socket is gone.
    started = false;
    clock.sleep(CONNECT_POLL);
    match attempt(home, &mut started, start, clock, &due, before_read) {
        Ok(hub) => return Ok(hub),
        Err(Poll::Race) => {}
        Err(Poll::Failed(error)) => return Err(error),
    }
    Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the hub closed the connection before `hub_hello`",
    ))
}

/// Whether a connect error means no hub runs, so the client starts one
/// and keeps polling: the socket is absent or nobody listens yet. Any
/// other error fails as is.
fn is_absent(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// Whether a handshake read error is the deadline passing, so the connect
/// fails `TimedOut`: any other read error fails as is.
fn is_hello_timeout(kind: io::ErrorKind) -> bool {
    matches!(kind, io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// Whether another handshake read fits before the deadline: at it or past
/// it none is left.
fn has_time_left(left: &Duration) -> bool {
    !left.is_zero()
}

/// Whether the hello line's last byte is a carriage return to strip: a
/// `\r\n` ending speaks `hub_hello` too.
fn strips_cr(last: Option<&u8>) -> bool {
    last == Some(&b'\r')
}

/// Whether the first line's kind is not the opening `hub_hello`.
fn rejects_hello(kind: &str) -> bool {
    kind != "hub_hello"
}

/// Whether the hello's schema version is not this build's.
fn rejects_schema(version: u32) -> bool {
    version != SCHEMA_VERSION
}

/// Whether a read-timeout error is the closed-peer refusal to ignore:
/// macOS refuses the option once the peer has closed, while its buffered
/// bytes stay readable and a read of the closed socket does not block.
fn is_closed_peer(kind: io::ErrorKind) -> bool {
    kind == io::ErrorKind::InvalidInput
}

/// What one handshake byte-read means.
enum HelloRead {
    /// One byte towards the line.
    Byte,
    /// End of stream before the line: the idle-exit race.
    Race,
    /// The deadline passed while reading.
    TimedOut,
    /// Any other read failure.
    Failed(io::Error),
}

/// Classifies one handshake byte-read, so tests inject each outcome.
fn classify_hello_read(read: io::Result<usize>) -> HelloRead {
    match read {
        Ok(0) => HelloRead::Race,
        Ok(_) => HelloRead::Byte,
        Err(error) if is_hello_timeout(error.kind()) => HelloRead::TimedOut,
        Err(error) => HelloRead::Failed(error),
    }
}

/// Folds a `set_read_timeout` result, ignoring the closed-peer refusal,
/// so tests inject each outcome.
fn ignore_closed_peer(set: io::Result<()>) -> io::Result<()> {
    match set {
        Err(error) if is_closed_peer(error.kind()) => Ok(()),
        other => other,
    }
}

enum Poll {
    /// EOF before `hub_hello`: retry the whole connect once.
    Race,
    /// No retry: the error.
    Failed(io::Error),
}

/// Polls the hub's socket until it accepts and speaks `hub_hello`. The
/// bind wait ends at `due`'s deadline: the shared one for `At`, one
/// [`CONNECT_DEADLINE`] from this attempt's start for `Within`.
fn attempt(
    home: &Path,
    started: &mut bool,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
    due: &Due,
    before_read: &mut dyn FnMut(),
) -> Result<Hub, Poll> {
    let socket = home.join("run").join("hub");
    let bind = due.bind_deadline(clock.now());
    let bind_named = match due {
        Due::At { total, .. } => *total,
        Due::Within(_) => CONNECT_DEADLINE,
    };
    loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => {
                let hello = due.hello_deadline(clock.now());
                return read_hello(stream, &socket, hello, due.named(), clock, before_read);
            }
            Err(error) if is_absent(error.kind()) => {
                if !*started {
                    start().map_err(Poll::Failed)?;
                    *started = true;
                }
                if clock.now() >= bind {
                    return Err(Poll::Failed(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "the hub did not bind {} in {} s",
                            socket.display(),
                            bind_named.as_secs_f64()
                        ),
                    )));
                }
                clock.sleep(CONNECT_POLL);
            }
            Err(error) => return Err(Poll::Failed(error)),
        }
    }
}

/// Reads the first line byte by byte, without buffered over-read, and
/// requires `hub_hello` on this build's `schema_version`. The line is due
/// by `deadline` on `clock`, fixed when the socket accepted, or with no
/// timeout when the accept plus the hello wait overflowed: before each
/// read the stream's read timeout is set to the time left, and with none
/// left the read fails `TimedOut`. `named` names the deadline in the
/// timeout message. `before_read` runs after the time-left check, just
/// before each read. The timeout is cleared once the line is read.
fn read_hello(
    mut stream: UnixStream,
    socket: &Path,
    deadline: Option<Instant>,
    named: Duration,
    clock: &dyn Clock,
    before_read: &mut dyn FnMut(),
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
        let left = match deadline {
            Some(deadline) => Some(
                deadline
                    .checked_duration_since(clock.now())
                    .filter(has_time_left)
                    .ok_or_else(timed_out)?,
            ),
            None => None,
        };
        set_read_timeout(&stream, left).map_err(Poll::Failed)?;
        before_read();
        match classify_hello_read(stream.read(&mut byte)) {
            HelloRead::Race => return Err(Poll::Race),
            HelloRead::TimedOut => return Err(timed_out()),
            HelloRead::Failed(error) => return Err(Poll::Failed(error)),
            HelloRead::Byte => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
        }
    }
    set_read_timeout(&stream, None).map_err(Poll::Failed)?;
    // A carriage return ends the line too.
    while strips_cr(buf.last()) {
        buf.pop();
    }
    let hello: HubLine = serde_json::from_slice(&buf).map_err(|_| {
        Poll::Failed(io::Error::new(
            io::ErrorKind::InvalidData,
            "the hub did not speak `hub_hello` first",
        ))
    })?;
    if rejects_hello(&hello.kind) {
        return Err(Poll::Failed(io::Error::new(
            io::ErrorKind::InvalidData,
            "the hub did not speak `hub_hello` first",
        )));
    }
    if rejects_schema(hello.schema_version) {
        return Err(Poll::Failed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the hub runs schema version {}, this Fiber runs schema version {SCHEMA_VERSION}; \
                 update Fiber or restart the hub, then reconnect",
                hello.schema_version,
            ),
        )));
    }
    Ok((stream, hello))
}

/// Sets `stream`'s read timeout, ignoring the closed-peer refusal (see
/// [`is_closed_peer`]): macOS refuses the option once the peer has closed,
/// while its buffered bytes stay readable and a read of the closed socket
/// does not block.
fn set_read_timeout(stream: &UnixStream, timeout: Option<Duration>) -> io::Result<()> {
    ignore_closed_peer(stream.set_read_timeout(timeout))
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
