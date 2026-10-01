//! Sending a request over HTTP, on a socket Fiber owns so another thread can
//! close it (`docs/architecture.md`, "Cancellation"). ureq runs behind a
//! connector that keeps a handle to each `TcpStream` it opens; shutting that
//! handle down ends a read blocked inside ureq, under TLS too.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ureq::Agent;
use ureq::config::Config;
use ureq::tls::{RootCerts, TlsConfig};
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, RustlsConnector, Transport,
};

use crate::Error;

/// One call's cancellation: whether it was cancelled, and its open socket.
#[derive(Debug, Default)]
pub(crate) struct Cancel {
    state: Mutex<CancelState>,
}

#[derive(Debug, Default)]
struct CancelState {
    cancelled: bool,
    socket: Option<TcpStream>,
}

impl Cancel {
    /// Marks the call cancelled and closes its socket, if one is open. A
    /// socket opened later is closed as it opens.
    pub(crate) fn cancel(&self) {
        let mut state = self.lock();
        state.cancelled = true;
        if let Some(socket) = state.socket.take() {
            // A socket the peer already closed fails to shut down, and is
            // closed either way.
            let _closed = socket.shutdown(Shutdown::Both);
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.lock().cancelled
    }

    /// Keeps a handle to `socket`, or refuses it once the call is cancelled.
    fn keep(&self, socket: &TcpStream) -> io::Result<()> {
        let mut state = self.lock();
        if state.cancelled {
            return Err(io::Error::other("the call was cancelled"));
        }
        state.socket = Some(socket.try_clone()?);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, CancelState> {
        // Release builds abort on panic, so no holder can poison the lock.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// POSTs `body` to `url` and returns the response body to read, or the
/// failure a non-2xx status reports.
pub(crate) fn post(
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    cancel: &Arc<Cancel>,
) -> Result<impl Read + use<>, Error> {
    // A call cancelled before it starts never resolves or connects.
    if cancel.is_cancelled() {
        return Err(Error::Connection("the call was cancelled".into()));
    }
    let tls = TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    let config = Config::builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .max_redirects(0)
        .build();
    // One agent per call, so its connector keeps this call's socket.
    // ponytail: builds the TLS config per call; share one agent with a
    // per-call socket slot if the handshake setup shows in a profile.
    let connector = KeepSocket(Arc::clone(cancel)).chain(RustlsConnector::default());
    let agent = Agent::with_parts(config, connector, DefaultResolver::default());
    let mut request = agent.post(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send(body)
        .map_err(|e| Error::Connection(e.to_string()))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim().to_ascii_lowercase())
        };
        let retry_after = header("retry-after").and_then(|v| v.parse::<f64>().ok());
        let should_retry = header("x-should-retry").and_then(|v| v.parse::<bool>().ok());
        let body = response.into_body().read_to_string().unwrap_or_default();
        return Err(Error::Status {
            status,
            body,
            retry_after,
            should_retry,
        });
    }
    Ok(response.into_body().into_reader())
}

/// The connector that opens the socket and keeps a handle to it.
#[derive(Debug)]
struct KeepSocket(Arc<Cancel>);

impl Connector<()> for KeepSocket {
    type Out = Socket;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _chained: Option<()>,
    ) -> Result<Option<Socket>, ureq::Error> {
        // ponytail: a connect blocked on an unreachable address is not
        // cancellable; the cancel lands as soon as it returns.
        let addrs: Vec<_> = details.addrs.iter().copied().collect();
        let stream = TcpStream::connect(addrs.as_slice())?;
        if details.config.no_delay() {
            stream.set_nodelay(true)?;
        }
        self.0.keep(&stream)?;
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        Ok(Some(Socket { stream, buffers }))
    }
}

/// A plain TCP transport over the kept socket.
struct Socket {
    stream: TcpStream,
    buffers: LazyBuffers,
}

impl fmt::Debug for Socket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Socket")
            .field("peer", &self.stream.peer_addr().ok())
            .finish()
    }
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
        Ok(read > 0)
    }

    fn is_open(&mut self) -> bool {
        true
    }
}
