//! The hub client (`docs/invocation.md`, "The hub"): connecting to
//! `run/hub`, starting one when none runs.
//!
//! A client starts the hub when none is running, then retries connecting
//! on the injected clock. The first line is read byte by byte, without
//! buffered over-read, and must be `hub_hello`: EOF before it is the
//! idle-exit race, and the whole connect is retried once.

use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use contract::HubLine;
use contract::clock::Clock;

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
/// `hub_hello` it spoke first.
pub fn connect(
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
) -> io::Result<Hub> {
    let mut started = false;
    // EOF before `hub_hello` is the idle-exit race: the whole connect is
    // retried once.
    for _ in 0..2 {
        match poll(home, &mut started, start, clock) {
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
) -> Result<Hub, Poll> {
    let socket = home.join("run").join("hub");
    let deadline = clock.now() + CONNECT_DEADLINE;
    loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => return read_hello(stream),
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
/// requires `hub_hello`.
fn read_hello(mut stream: UnixStream) -> Result<Hub, Poll> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Err(Poll::Race),
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
            Err(error) => return Err(Poll::Failed(error)),
        }
    }
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
    Ok((stream, hello))
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
