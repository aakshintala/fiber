//! The stoppable read: a reader over a Unix socket that a [`Stop`] ends on
//! demand, shared by the crates whose reader threads something joins.
//!
//! A blocked read woken only by `shutdown` can miss its wakeup on macOS
//! (#1877): `shutdown(Both)` on one end succeeds while a read already
//! blocked on the other end is never woken. So each [`Reader::read`] polls
//! the socket together with a wake pipe, and [`Stop::stop`] closes the
//! pipe's write end. Nothing ever writes to the pipe or reads from it:
//! once its write end has closed, the read end reports ready on every later
//! poll and stays ready, so a stop before the poll, during it or between
//! reads all end the next `read`. An idle reader waits only in `poll` with
//! no timeout, so it causes no context switches (`docs/performance.md`).

use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use rustix::event::{PollFd, PollFlags, poll};

/// Makes the stoppable reader over `inner`, and the [`Stop`] that ends it.
///
/// Fails with [`Error::Pipe`] when the wake pipe cannot be made, before
/// anything is published: no thread starts without its `Stop` stored.
///
/// Precondition: the returned [`Reader`] is the stream's only reader.
/// Clones may write and may shut the socket down, but never read, so
/// nothing can take the readiness that `poll` reported before the `read`
/// that follows it.
pub fn reader(inner: UnixStream) -> Result<(Reader, Stop), Error> {
    let (woken, wake) = io::pipe().map_err(Error::Pipe)?;
    Ok((
        Reader { inner, woken },
        Stop {
            wake: Mutex::new(Some(wake)),
        },
    ))
}

/// A reader over a Unix socket that [`Stop::stop`] ends on demand. Each
/// `read` polls the socket and the wake pipe's read end with no timeout:
/// when the pipe is ready the read was stopped, and otherwise one `read`
/// on the socket follows, whose result is returned unchanged. The pipe is
/// never read from and never written to; its read end closes when the
/// `Reader` drops.
pub struct Reader {
    inner: UnixStream,
    woken: io::PipeReader,
}

impl Reader {
    /// The wrapped stream. Clones from it may write and may shut the socket
    /// down, but never read: this `Reader` stays the stream's only reader.
    pub fn get_ref(&self) -> &UnixStream {
        &self.inner
    }
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut fds = [
                PollFd::new(&self.woken, PollFlags::IN),
                PollFd::new(&self.inner, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(io::Error::from(error)),
            }
            let [woken, _stream] = &fds;
            // Any readiness on the pipe is the stop: nothing is ever
            // written to it, so only its write end closing wakes it, and a
            // closed pipe may report `POLLHUP` alone. The stop wins over
            // pending data, so a peer that keeps writing cannot delay it.
            // Bytes already read are kept: this returns without reading.
            if !woken.revents().is_empty() {
                return Err(stopped());
            }
            return self.inner.read(buf);
        }
    }
}

/// Ends a [`Reader`]'s blocked read on demand: [`Stop::stop`] closes the
/// wake pipe's write end, and the reader's next `read` reports the stop.
/// Dropping a `Stop` stops its reader too. Every owner keeps its `Stop`
/// exactly as long as the reader may run, so no reader ends early.
///
/// A `Stop` is a leaf lock: `stop` takes no other lock, so calling it under
/// another lock cannot deadlock.
pub struct Stop {
    wake: Mutex<Option<io::PipeWriter>>,
}

impl Stop {
    /// Ends the reader's next `read` with the stop error. Idempotent: a
    /// second call does nothing. Neither call fails. The only thing it can
    /// wait for is a concurrent `stop` on the same `Stop`, for the length
    /// of that call's take: no I/O runs under the lock, and the close
    /// happens after the lock is released.
    pub fn stop(&self) {
        let writer = { crate::lock(&self.wake).take() };
        drop(writer);
    }
}

/// What [`reader`] and a stopped [`Reader::read`] report. It names no stable
/// code (`docs/code-quality.md`, "Errors"): a caller that reports one maps
/// each case to its own code, with no wildcard arm.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The wake pipe could not be made.
    #[error("the stop pipe could not be made: {0}")]
    Pipe(#[source] io::Error),
    /// The read was stopped. A stopped read reports it in an [`io::Error`]
    /// of kind [`io::ErrorKind::ConnectionAborted`]: not `Ok(0)`, which
    /// `read_until` would hand over as a whole line, and not `Interrupted`,
    /// which `read_until` retries and would loop on forever.
    #[error("the read was stopped")]
    Stopped,
}

/// The error a stopped read returns.
fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, Error::Stopped)
}

#[cfg(test)]
#[path = "stoppable_tests.rs"]
mod tests;
