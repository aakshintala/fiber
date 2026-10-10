//! The connector that opens the socket and keeps a handle to it, and the
//! transport over that socket.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, Transport,
};

use crate::{Error, Keep, Limits};

/// The connector that opens the socket and keeps a handle to it. A tunnel
/// the proxy step opened passes through untouched: the socket kept while
/// opening the proxy connection is already the one a stop must close.
#[derive(Debug)]
pub(crate) struct KeepSocket<K>(pub(crate) Arc<K>, pub(crate) Limits);

impl<K: Keep> KeepSocket<K> {
    pub(crate) fn new(keep: Arc<K>, limits: Limits) -> Self {
        Self(keep, limits)
    }
}

impl<K: Keep> Connector<Either<(), Box<dyn Transport>>> for KeepSocket<K> {
    type Out = Either<Box<dyn Transport>, Socket>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<Either<(), Box<dyn Transport>>>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        // A connect blocked on an unreachable address waits out that
        // address's attempt; the stop lands when the attempt returns, at
        // most `Limits::connect` later. Connect from a cancellable thread
        // if a stop stuck on connect is reported.
        if let Some(Either::B(tunnel)) = chained {
            return Ok(Some(Either::A(tunnel)));
        }
        let stream = open(&details.addrs, self.0.as_ref(), self.1, details.timeout)?;
        if details.config.no_delay() {
            stream.set_nodelay(true)?;
        }
        self.0.keep(&stream)?;
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        Ok(Some(Either::B(Socket {
            stream,
            buffers,
            open: true,
            idle: self.1.idle(),
        })))
    }
}

/// Opens a socket to the first address that connects, in order. Before each
/// attempt the keep is asked whether the call is stopped; once it is, no
/// further attempt starts. A stop that lands during an attempt is refused by
/// [`Keep::keep`] when the attempt returns. Each attempt waits at most the
/// connect limit, or ureq's bound when strictly shorter. When no address
/// connects, the last failure is returned.
fn open(
    addrs: &[SocketAddr],
    keep: &impl Keep,
    limits: Limits,
    timeout: NextTimeout,
) -> Result<TcpStream, ureq::Error> {
    let (bound, ureq_wins) = connect_bound(limits.connect(), timeout);
    let mut last: Option<ureq::Error> = None;
    for addr in addrs {
        if keep.is_stopped() {
            return Err(ureq::Error::Io(io::Error::other(Error::Stopped)));
        }
        match TcpStream::connect_timeout(addr, bound) {
            Ok(stream) => return Ok(stream),
            Err(failed) if failed.kind() == io::ErrorKind::TimedOut && ureq_wins => {
                last = Some(ureq::Error::Timeout(timeout.reason));
            }
            Err(failed) if failed.kind() == io::ErrorKind::TimedOut => {
                last = Some(ureq::Error::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    Error::ConnectTimedOut {
                        addr: *addr,
                        limit: limits.connect(),
                    },
                )));
            }
            Err(failed) => last = Some(ureq::Error::Io(failed)),
        }
    }
    match last {
        Some(failed) => Err(failed),
        // The error `TcpStream::connect` gives for an empty address list.
        None => match TcpStream::connect(addrs) {
            Ok(stream) => Ok(stream),
            Err(failed) => Err(ureq::Error::Io(failed)),
        },
    }
}

/// How long one connect attempt may wait: ureq's bound when strictly
/// shorter, else the connect limit. The flag tells whether a timeout is
/// ureq's, so the error keeps its reason and ureq's state machine holds.
fn connect_bound(connect: Duration, timeout: NextTimeout) -> (Duration, bool) {
    match timeout.not_zero() {
        Some(ureq) if *ureq < connect => (*ureq, true),
        _ => (connect, false),
    }
}

/// How long one read or write syscall may wait: ureq's bound when strictly
/// shorter, else the idle limit. The flag tells whether a timeout is ureq's.
/// Each syscall is bounded afresh, so a transfer that keeps moving may take
/// longer in total than either bound.
fn io_bound(idle: Duration, timeout: NextTimeout) -> (Duration, bool) {
    match timeout.not_zero() {
        Some(ureq) if *ureq < idle => (*ureq, true),
        _ => (idle, false),
    }
}

/// A plain TCP transport over the kept socket.
#[derive(Debug)]
pub(crate) struct Socket {
    stream: TcpStream,
    buffers: LazyBuffers,
    /// False once a read found the peer had closed.
    open: bool,
    idle: Duration,
}

impl Socket {
    /// Maps a read or write failure to the bound that expired: ureq's
    /// timeout keeps its reason, while the idle limit shuts the socket down
    /// both ways before reporting the stall, so the peer and the kept clone
    /// see the end at once.
    fn timed_out(&self, error: io::Error, timeout: NextTimeout, ureq_wins: bool) -> ureq::Error {
        if error.kind() != io::ErrorKind::WouldBlock && error.kind() != io::ErrorKind::TimedOut {
            return ureq::Error::Io(error);
        }
        if ureq_wins {
            return ureq::Error::Timeout(timeout.reason);
        }
        let _closed = self.stream.shutdown(Shutdown::Both);
        ureq::Error::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            Error::Stalled { idle: self.idle },
        ))
    }
}

impl Transport for Socket {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let (bound, ureq_wins) = io_bound(self.idle, timeout);
        self.stream.set_write_timeout(Some(bound))?;
        let output = self
            .buffers
            .output()
            .get(..amount)
            .ok_or_else(|| io::Error::other("ureq asked to send more than its buffer holds"))?;
        match self.stream.write_all(output) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.timed_out(error, timeout, ureq_wins)),
        }
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let (bound, ureq_wins) = io_bound(self.idle, timeout);
        self.stream.set_read_timeout(Some(bound))?;
        let input = self.buffers.input_append_buf();
        let read = match self.stream.read(input) {
            Ok(read) => read,
            Err(error) => return Err(self.timed_out(error, timeout, ureq_wins)),
        };
        self.buffers.input_appended(read);
        // No bytes is the peer closing: ureq reads `false` as no progress.
        self.open = read != 0;
        Ok(self.open)
    }

    fn is_open(&mut self) -> bool {
        self.open
    }
}

#[cfg(test)]
#[path = "socket_tests.rs"]
mod tests;
