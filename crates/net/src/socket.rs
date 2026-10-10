//! The connector that opens the socket and keeps a handle to it, and the
//! transport over that socket.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, Transport,
};

use crate::{Error, Keep};

/// The connector that opens the socket and keeps a handle to it. A tunnel
/// the proxy step opened passes through untouched: the socket kept while
/// opening the proxy connection is already the one a stop must close.
#[derive(Debug)]
pub(crate) struct KeepSocket<K>(pub(crate) Arc<K>);

impl<K: Keep> KeepSocket<K> {
    pub(crate) fn new(keep: Arc<K>) -> Self {
        Self(keep)
    }
}

impl<K: Keep> Connector<Either<(), Box<dyn Transport>>> for KeepSocket<K> {
    type Out = Either<Box<dyn Transport>, Socket>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<Either<(), Box<dyn Transport>>>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        // A connect blocked on an unreachable address is not cancellable;
        // the stop lands as soon as it returns. Connect with a timeout or
        // from a cancellable thread if a stop stuck on connect is reported.
        if let Some(Either::B(tunnel)) = chained {
            return Ok(Some(Either::A(tunnel)));
        }
        let stream = open(&details.addrs, self.0.as_ref())?;
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
        })))
    }
}

/// Opens a socket to the first address that connects, in order. Before each
/// attempt the keep is asked whether the call is stopped; once it is, no
/// further attempt starts. A stop that lands during an attempt is refused by
/// [`Keep::keep`] when the attempt returns. When no address connects, the
/// last failure is returned.
fn open(addrs: &[SocketAddr], keep: &impl Keep) -> io::Result<TcpStream> {
    let mut last = None;
    for addr in addrs {
        if keep.is_stopped() {
            return Err(io::Error::other(Error::Stopped));
        }
        match TcpStream::connect(*addr) {
            Ok(stream) => return Ok(stream),
            Err(failed) => last = Some(failed),
        }
    }
    match last {
        Some(failed) => Err(failed),
        // The error `TcpStream::connect` gives for an empty address list.
        None => TcpStream::connect(addrs),
    }
}

/// A plain TCP transport over the kept socket.
#[derive(Debug)]
pub(crate) struct Socket {
    stream: TcpStream,
    buffers: LazyBuffers,
    /// False once a read found the peer had closed.
    open: bool,
}

impl Transport for Socket {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, _timeout: NextTimeout) -> Result<(), ureq::Error> {
        let output = self
            .buffers
            .output()
            .get(..amount)
            .ok_or_else(|| io::Error::other("ureq asked to send more than its buffer holds"))?;
        self.stream.write_all(output)?;
        Ok(())
    }

    fn await_input(&mut self, _timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let input = self.buffers.input_append_buf();
        let read = self.stream.read(input)?;
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
