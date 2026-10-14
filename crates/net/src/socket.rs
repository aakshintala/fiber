//! The connector that opens the socket and keeps a handle to it, and the
//! transport over that socket.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, Transport,
};

use contract::diag::{Op, Timing};

use crate::{Error, Keep, Limits};

/// The connector that opens the socket and keeps a handle to it. A tunnel
/// the proxy step opened passes through untouched: the socket kept while
/// opening the proxy connection is already the one a stop must close.
/// The optional callable times one actual attempt group: it starts
/// immediately before the first socket attempt and its guard drops once
/// the group resolves, after the socket is kept. The callable itself is
/// not debug output, so the [`Debug`] form names only whether one is
/// installed, never any address or secret it might close over.
pub(crate) struct KeepSocket(pub(crate) Arc<dyn Keep>, pub(crate) Limits, Option<Timing>);

impl std::fmt::Debug for KeepSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeepSocket")
            .field("keep", &self.0)
            .field("limits", &self.1)
            .field("timing", &self.2.is_some())
            .finish()
    }
}

impl KeepSocket {
    pub(crate) fn new(keep: Arc<dyn Keep>, limits: Limits) -> Self {
        Self(keep, limits, None)
    }

    /// Times the connector's attempt groups through `timing`: building
    /// the connector starts no scope, and each group owns its guard.
    pub(crate) fn with_timing(self, timing: Timing) -> Self {
        Self(self.0, self.1, Some(timing))
    }
}

impl Connector<Either<(), Box<dyn Transport>>> for KeepSocket {
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
        // The plain path keeps today's open; the observed path times one
        // group through the installed callable. Either way the scope ends
        // after the socket is kept, before any buffer exists: a failure
        // in between drops the guard with the stream, once.
        let (stream, guard) = match &self.2 {
            None => (
                open(&details.addrs, self.0.as_ref(), self.1, details.timeout)?,
                None,
            ),
            Some(timing) => open_timed(
                &details.addrs,
                self.0.as_ref(),
                self.1,
                details.timeout,
                Some(timing),
            )?,
        };
        if details.config.no_delay() {
            stream.set_nodelay(true)?;
        }
        self.0.keep(&stream)?;
        drop(guard);
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

/// Opens a socket to the first address that connects, in order, without
/// timing the group: the plain path. Shares the walk with [`open_timed`].
fn open(
    addrs: &[SocketAddr],
    keep: &dyn Keep,
    limits: Limits,
    timeout: NextTimeout,
) -> Result<TcpStream, ureq::Error> {
    open_timed(addrs, keep, limits, timeout, None).map(|(stream, _)| stream)
}

/// Opens a socket to the first address that connects, in order, timing
/// one group through `timing` when supplied and returning the group's
/// guard with the socket. The callable runs at most once, immediately
/// before the first socket attempt after its stop check: an empty address
/// list and a stop before the first attempt start no scope, and a `None`
/// answer never rearms on a later address. A failed group drops its guard
/// on the way out, once; a successful one hands it to the caller, which
/// drops it after the socket is kept. Before each attempt the keep is
/// asked whether the call is stopped; once it is, no further attempt
/// starts. A stop that lands during an attempt is refused by [`Keep::keep`]
/// when the attempt returns. Each attempt waits at most the connect limit,
/// or ureq's bound when strictly shorter. When no address connects, the
/// last failure is returned.
fn open_timed(
    addrs: &[SocketAddr],
    keep: &dyn Keep,
    limits: Limits,
    timeout: NextTimeout,
    timing: Option<&Timing>,
) -> Result<(TcpStream, Option<Box<dyn Send>>), ureq::Error> {
    let (bound, ureq_wins) = connect_bound(limits.connect(), timeout);
    let mut last: Option<ureq::Error> = None;
    // Whether the scope started; distinct from whether starting answered
    // `Some`, so a `None` answer is final for the group.
    let mut started = false;
    let mut guard: Option<Box<dyn Send>> = None;
    for addr in addrs {
        if keep.is_stopped() {
            return Err(ureq::Error::Io(io::Error::other(Error::Stopped)));
        }
        if !started {
            started = true;
            guard = timing.and_then(|start| start(Op::NetConnect, None, None));
        }
        match TcpStream::connect_timeout(addr, bound) {
            Ok(stream) => return Ok((stream, guard)),
            Err(failed) => {
                let reason = ureq_wins.then_some(timeout.reason);
                last = Some(connect_error(failed, *addr, limits.connect(), reason));
            }
        }
    }
    match last {
        Some(failed) => Err(failed),
        // The error `TcpStream::connect` gives for an empty address list.
        None => match TcpStream::connect(addrs) {
            Ok(stream) => Ok((stream, guard)),
            Err(failed) => Err(ureq::Error::Io(failed)),
        },
    }
}

/// A failed connect attempt as the error ureq sees: a timeout is ureq's own
/// when `ureq_reason` is set, so ureq's state machine keeps its reason, and
/// the connect limit's otherwise; any other failure passes through.
fn connect_error(
    failed: io::Error,
    addr: SocketAddr,
    limit: Duration,
    ureq_reason: Option<ureq::Timeout>,
) -> ureq::Error {
    if failed.kind() != io::ErrorKind::TimedOut {
        return ureq::Error::Io(failed);
    }
    match ureq_reason {
        Some(reason) => ureq::Error::Timeout(reason),
        None => ureq::Error::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            Error::ConnectTimedOut { addr, limit },
        )),
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

/// Reads once, retrying a read a signal interrupted. Linux never restarts a
/// socket read that has a receive timeout set, even for a handler installed
/// with `SA_RESTART`, so a signal aimed at the process would otherwise fail
/// the call (signal(7), "Interruption of system calls and library functions
/// by signal handlers").
fn read_retrying(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(buf) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            done => return done,
        }
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
        let read = match read_retrying(&mut self.stream, input) {
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

#[cfg(test)]
#[path = "timing_tests.rs"]
mod timing_tests;
