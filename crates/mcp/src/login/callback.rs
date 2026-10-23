//! The login's loopback callback wait (`docs/mcp.md`, "What Fiber does
//! with MCP"): one `/callback` request on a bound loopback listener, read
//! on the injected clock so an idle or slow-drip connection cannot extend
//! the 15-minute wait. The single entry is [`wait_callback`]; everything
//! else is its helper, with the listener's own limits.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use contract::clock::Clock;

use super::{LoginError, is_known_error};

/// The longest request head the callback listener reads. Past it the
/// client gets a 431 and the listener serves the next connection.
pub(crate) const MAX_HEAD: usize = 8 * 1024;

/// How many reads in a row a callback connection may send nothing before
/// the listener drops it and serves the next. Browsers open idle
/// speculative connections, so silence on one cannot end the wait. The
/// count restarts whenever bytes arrive.
pub(crate) const SILENT_POLLS: u32 = 100;

/// One callback socket read's kernel timeout: short, so the injected
/// 900 s deadline is checked between reads and an idle or slow-drip
/// connection cannot extend the wait.
const READ_TIMEOUT: Duration = Duration::from_millis(20);

/// One accept poll's real-time pause between listener checks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// Test-only taps on the callback reader: each registered tap receives one
/// message per byte the reader consumes. The slow-drip test registers one
/// tap, so its first message proves the connection was accepted and each
/// later one releases the next drip byte; nothing else registers, and the
/// shipped binary holds no tap.
#[cfg(test)]
static READ_TAPS: std::sync::Mutex<Vec<std::sync::mpsc::Sender<()>>> =
    std::sync::Mutex::new(Vec::new());

/// Registers a tap on the callback reader's consumed bytes, for the
/// slow-drip test's handshake (`docs/testing.md`, "Waits and timeouts").
#[cfg(test)]
pub(crate) fn tap_reads() -> std::sync::mpsc::Receiver<()> {
    let (sent, received) = std::sync::mpsc::channel();
    support::lock(&READ_TAPS).push(sent);
    received
}

/// Sends one message to every read tap. A tap whose test went away hangs
/// up here, never in the reader.
#[cfg(test)]
fn signal_read() {
    if let Ok(taps) = READ_TAPS.lock() {
        for tap in taps.iter() {
            match tap.send(()) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// Test-only taps on the callback reader's silent polls: each registered
/// tap receives one message per read that finds no bytes. The pausing-head
/// test registers one tap, so its first message proves the wait polled
/// with no bytes before the rest of the head arrives; nothing else
/// registers, and the shipped binary holds no tap.
#[cfg(test)]
static SILENCE_TAPS: std::sync::Mutex<Vec<std::sync::mpsc::Sender<()>>> =
    std::sync::Mutex::new(Vec::new());

/// Registers a tap on the callback reader's silent polls, for the
/// pausing-head test's handshake (`docs/testing.md`, "Waits and timeouts").
#[cfg(test)]
pub(crate) fn tap_silence() -> std::sync::mpsc::Receiver<()> {
    let (sent, received) = std::sync::mpsc::channel();
    support::lock(&SILENCE_TAPS).push(sent);
    received
}

/// Sends one message to every silence tap. A tap whose test went away
/// hangs up here, never in the reader.
#[cfg(test)]
fn signal_silence() {
    if let Ok(taps) = SILENCE_TAPS.lock() {
        for tap in taps.iter() {
            match tap.send(()) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// A query string's parameters in order, percent-decoded with `+` as a
/// space.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect()
}

/// Percent-decodes one query value, with `+` as a space; the tests
/// read query values back through it.
pub(crate) fn decode(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => out.push(b' '),
            b'%' => {
                // Two hex digits make a byte; anything else leaves the
                // `%` as written and the next bytes to be read as
                // themselves.
                let mut ahead = bytes.clone();
                let hex = |next: Option<u8>| next.and_then(|b| char::from(b).to_digit(16));
                match (hex(ahead.next()), hex(ahead.next())) {
                    (Some(high), Some(low)) => {
                        if let Ok(value) = u8::try_from(high * 16 + low) {
                            out.push(value);
                            bytes = ahead;
                        } else {
                            out.push(b'%');
                        }
                    }
                    _ => out.push(b'%'),
                }
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The request line's target, when the head is a request.
fn request_target(head: &[u8]) -> Option<&str> {
    let line = head.split(|byte| *byte == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split_whitespace();
    let (_method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    (version.starts_with("HTTP/") && parts.next().is_none()).then_some(target)
}

fn respond(stream: &mut TcpStream, status: &str) {
    let body = if status.starts_with("200") {
        "<!doctype html><title>Fiber</title><p>You can close this tab and return to Fiber.</p>"
    } else {
        ""
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    match stream.write_all(reply.as_bytes()) {
        Ok(()) | Err(_) => {}
    }
}

pub(crate) enum Served {
    /// The wait is over with the authorization code.
    Code(String),
    /// The wait is over with a login failure.
    Failed(LoginError),
}

/// Answers one callback connection. `Some` ends the wait; `None` serves
/// the next connection. The callback tests call it directly with the
/// injected clock, so a mutant of its answer or cap fails without waiting
/// out a deadline. Test-only: production reaches [`answer_within`]
/// through [`wait_callback_within`].
#[cfg(test)]
pub(crate) fn answer(
    stream: TcpStream,
    clock: &dyn Clock,
    deadline: Instant,
    state: &str,
) -> Option<Served> {
    answer_within(stream, clock, deadline, state, SILENT_POLLS)
}

/// [`answer`] with the silent-poll count as a parameter, so tests can run
/// a small count while production passes [`SILENT_POLLS`].
pub(crate) fn answer_within(
    mut stream: TcpStream,
    clock: &dyn Clock,
    deadline: Instant,
    state: &str,
    silent_polls: u32,
) -> Option<Served> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    // The poll pause: one condvar wait, so the drain's park below never
    // spins and the fake clock still sees every park.
    let poll = (Mutex::new(()), Condvar::new());
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    let mut silent = 0;
    loop {
        // The whole head read is bounded by the injected deadline: an
        // idle or slow-drip connection cannot extend the wait.
        if clock.now() >= deadline {
            return Some(Served::Failed(LoginError::Timeout));
        }
        if head.len() >= MAX_HEAD {
            respond(&mut stream, "431 Request Header Fields Too Large");
            // The reply is written before the rest of the head is read:
            // closing with the request half-read resets the connection,
            // and the peer reads an empty reply instead of the 431. The
            // drain below reads to the head's end first, so the close
            // carries the 431 every time. It is bounded past one more cap
            // of bytes, and by the same injected deadline as the head
            // read, so a head that never ends still ends here, and a peer
            // that drips the refused head cannot extend the wait.
            while head.len() < 2 * MAX_HEAD {
                if clock.now() >= deadline {
                    return Some(Served::Failed(LoginError::Timeout));
                }
                match stream.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) => head.push(byte[0]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        // No bytes yet: park on the injected clock
                        // instead of ending the drain on the socket's
                        // kernel timeout, so a test's `await_parked`
                        // sees the drain and advancing the fake clock
                        // past the deadline ends it with the timeout.
                        // A closed, reset or refused connection still
                        // ends the drain at once below.
                        clock.wait_until(Some(deadline), &mut |bound| {
                            let pause = bound.map_or(POLL_PAUSE, |bound| bound.min(POLL_PAUSE));
                            let (lock, changed) = &poll;
                            if let Ok(guard) = lock.lock() {
                                let _waited = changed
                                    .wait_timeout(guard, pause)
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                            }
                        });
                    }
                    Err(_) => break,
                }
                if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
                    break;
                }
            }
            return None;
        }
        match stream.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) => {
                head.push(byte[0]);
                silent = 0;
                #[cfg(test)]
                signal_read();
            }
            Err(_) => {
                silent += 1;
                #[cfg(test)]
                signal_silence();
                if silent >= silent_polls {
                    return None;
                }
            }
        }
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
    }
    let Some(target) = request_target(&head) else {
        respond(&mut stream, "400 Bad Request");
        return Some(Served::Failed(LoginError::Callback(
            "callback request".to_owned(),
        )));
    };
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    if path != "/callback" {
        respond(&mut stream, "404 Not Found");
        return None;
    }
    let query = target.split_once('?').map_or("", |(_, query)| query);
    let pairs = parse_query(query);
    let find = |name| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, v)| v.as_str())
    };
    respond(&mut stream, "200 OK");
    if let Some(error) = find("error") {
        // Only the seven codes print; any other value, and never the
        // description or an echo of `state`, prints the generic sentence.
        if is_known_error(error) {
            return Some(Served::Failed(LoginError::Denied(error.to_owned())));
        }
        return Some(Served::Failed(LoginError::Denied(String::new())));
    }
    let (Some(code), Some(returned)) = (find("code"), find("state")) else {
        return Some(Served::Failed(LoginError::Callback(
            "callback parameters".to_owned(),
        )));
    };
    if code.is_empty() || returned != state {
        return Some(Served::Failed(LoginError::Callback(
            "callback state".to_owned(),
        )));
    }
    Some(Served::Code(code.to_owned()))
}

/// Waits for the first `/callback` request, on the injected clock. A
/// request for another path gets 404 and the wait continues; the first
/// `/callback` request ends it.
pub(crate) fn wait_callback(
    listener: &TcpListener,
    clock: &dyn Clock,
    deadline: Instant,
    state: &str,
) -> Result<String, LoginError> {
    wait_callback_within(listener, clock, deadline, state, SILENT_POLLS)
}

/// [`wait_callback`] with the silent-poll count as a parameter, so tests
/// can run a small count while production passes [`SILENT_POLLS`].
pub(crate) fn wait_callback_within(
    listener: &TcpListener,
    clock: &dyn Clock,
    deadline: Instant,
    state: &str,
    silent_polls: u32,
) -> Result<String, LoginError> {
    // The poll pause: one condvar wait, so the idle loop never spins
    // and the fake clock still sees every park below.
    let poll = (std::sync::Mutex::new(()), std::sync::Condvar::new());
    loop {
        if clock.now() >= deadline {
            return Err(LoginError::Timeout);
        }
        match listener.accept() {
            Ok((stream, _)) => match answer_within(stream, clock, deadline, state, silent_polls) {
                None => {}
                Some(Served::Code(code)) => return Ok(code),
                Some(Served::Failed(error)) => return Err(error),
            },
            Err(_) => {
                // Parked on the clock, so a test's `await_parked` sees
                // the wait; the pause is the shorter of the clock's
                // bound and the poll, so the deadline still ends it.
                clock.wait_until(Some(deadline), &mut |bound| {
                    let pause = bound.map_or(POLL_PAUSE, |bound| bound.min(POLL_PAUSE));
                    let (lock, changed) = &poll;
                    if let Ok(guard) = lock.lock() {
                        let _waited = changed
                            .wait_timeout(guard, pause)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                });
            }
        }
    }
}
