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
use std::time::Duration;

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
/// when none runs. `start` runs once at most. Returns the stream and the
/// `hub_hello` it spoke first, which must arrive within [`CONNECT_DEADLINE`].
pub fn connect(
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
) -> io::Result<Hub> {
    connect_within(home, start, clock, CONNECT_DEADLINE)
}

/// [`connect`], with `hub_hello` due within `hello_within` of the socket
/// accepting, on `clock`. Past it the connect fails `TimedOut`.
pub fn connect_within(
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
    hello_within: Duration,
) -> io::Result<Hub> {
    let mut started = false;
    // EOF before `hub_hello` is the idle-exit race: the whole connect is
    // retried once.
    for _ in 0..2 {
        match poll(home, &mut started, start, clock, hello_within) {
            Ok(hub) => return Ok(hub),
            Err(Poll::Race) => {}
            Err(Poll::Failed(error)) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the hub closed the connection before `hub_hello`",
    ))
}

enum Poll {
    /// EOF before `hub_hello`: retry the whole connect once.
    Race,
    /// No retry: the error.
    Failed(io::Error),
}

/// Polls the hub's socket until it accepts and speaks `hub_hello`.
fn poll(
    home: &Path,
    started: &mut bool,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
    hello_within: Duration,
) -> Result<Hub, Poll> {
    let socket = home.join("run").join("hub");
    let deadline = clock.now() + CONNECT_DEADLINE;
    loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => return read_hello_with(stream, &socket, hello_within, clock, &mut || {}),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                if !*started {
                    start().map_err(Poll::Failed)?;
                    *started = true;
                }
                if clock.now() >= deadline {
                    return Err(Poll::Failed(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("the hub did not bind {} in 5 s", socket.display()),
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
/// within `within` of the call on `clock`: before each read the stream's
/// read timeout is set to the time left, and with none left the read fails
/// `TimedOut`. `before_read` runs just before each read. The timeout is
/// cleared once the line is read.
fn read_hello_with(
    mut stream: UnixStream,
    socket: &Path,
    within: Duration,
    clock: &dyn Clock,
    before_read: &mut dyn FnMut(),
) -> Result<Hub, Poll> {
    let deadline = clock.now().checked_add(within);
    let timed_out = || {
        Poll::Failed(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "the hub at {} accepted but did not say hub_hello in {} s",
                socket.display(),
                within.as_secs_f64()
            ),
        ))
    };
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let left = match deadline {
            Some(deadline) => match deadline.checked_duration_since(clock.now()) {
                Some(left) if !left.is_zero() => Some(left),
                _ => return Err(timed_out()),
            },
            None => None,
        };
        set_read_timeout(&stream, left).map_err(Poll::Failed)?;
        before_read();
        match stream.read(&mut byte) {
            Ok(0) => return Err(Poll::Race),
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(timed_out());
            }
            Err(error) => return Err(Poll::Failed(error)),
        }
    }
    set_read_timeout(&stream, None).map_err(Poll::Failed)?;
    // A carriage return ends the line too.
    while buf.last() == Some(&b'\r') {
        buf.pop();
    }
    let hello: HubLine = serde_json::from_slice(&buf).map_err(|_| {
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
    Ok((stream, hello))
}

/// Sets `stream`'s read timeout. `InvalidInput` is ignored: macOS refuses
/// the option once the peer has closed, while its buffered bytes stay
/// readable and a read of the closed socket does not block.
fn set_read_timeout(stream: &UnixStream, timeout: Option<Duration>) -> io::Result<()> {
    match stream.set_read_timeout(timeout) {
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        other => other,
    }
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
