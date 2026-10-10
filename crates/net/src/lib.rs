//! The TLS configuration every HTTPS call uses, the connector that keeps
//! each request's socket so another thread can close it, the limits that
//! bound each call's socket, and the error its socket reports
//! (`docs/architecture.md`, "The modules", "Cancellation";
//! `docs/dependencies.md`, "Root certificates", "Proxies";
//! `docs/model-routing.md`, "When a model call fails"). [`config`]
//! carries the platform verifier, or on Linux Mozilla's roots when the
//! system store is empty, and nothing else; each caller sets its own
//! proxy and request policy on top.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig};
use ureq::unversioned::resolver::Resolver;
use ureq::unversioned::transport::{ConnectProxyConnector, Connector, RustlsConnector};

use crate::socket::KeepSocket;

mod socket;

/// The TLS configuration every HTTPS call uses: the platform verifier, or
/// on Linux Mozilla's roots compiled in through ureq when the system store
/// has no certificates (`docs/dependencies.md`, "Root certificates").
pub fn tls_config() -> TlsConfig {
    tls_config_with(
        &STORE_IS_EMPTY,
        platform_certificate_count,
        RootCerts::WebPki,
    )
}

/// Whether the Linux system store came back empty. The store is loaded once
/// per process and the answer is kept here, so later calls reuse it.
static STORE_IS_EMPTY: OnceLock<bool> = OnceLock::new();

/// The TLS configuration for `fallback` when the cached empty-store answer
/// says the store is empty, else the platform verifier. `load` counts the
/// store's certificates; it runs at most once per `cache`.
fn tls_config_with(
    cache: &OnceLock<bool>,
    load: impl FnOnce() -> usize,
    fallback: RootCerts,
) -> TlsConfig {
    let empty = cache.get_or_init(|| load() == 0);
    TlsConfig::builder()
        .root_certs(if *empty {
            fallback
        } else {
            RootCerts::PlatformVerifier
        })
        .build()
}

/// How many certificates the platform's loader returns: the Linux system
/// store's count, or 1 elsewhere, where the platform verifier always runs.
/// One function with inner `cfg` blocks, so a mutant here is always
/// compiled and never silently gated away.
fn platform_certificate_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        rustls_native_certs::load_native_certs().certs.len()
    }
    #[cfg(not(target_os = "linux"))]
    {
        1
    }
}

/// The agent config every HTTPS call starts from: [`tls_config`] and nothing
/// else. The caller sets its own proxy and request policy, so the proxy
/// default stays ureq's, read from the environment.
pub fn config() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::config::Config::builder().tls_config(tls_config())
}

/// Builds one agent per call over the shared connector, which keeps the
/// call's socket so another thread can close it. The proxy step runs before
/// the socket step: it opens the proxy connection by re-running the chain,
/// so the socket the connector keeps is the proxy's, and a stop still
/// closes the tunnel.
// debt: builds the TLS config per call; share one agent with a per-call
// socket slot if the handshake setup shows in a profile.
pub fn agent<K: Keep>(
    config: ureq::config::Config,
    keep: Arc<K>,
    resolver: impl Resolver,
    limits: Limits,
) -> ureq::Agent {
    let connector = ConnectProxyConnector::default().chain(KeepSocket::new(keep, limits));
    let connector = connector.chain(RustlsConnector::default());
    ureq::Agent::with_parts(config, connector, resolver)
}

/// How long one call may wait on its socket: each resolved address gets
/// [`Limits::connect`] to connect, and each read or write syscall gets
/// [`Limits::idle`] to move a byte, from the request being sent through
/// the end of the stream (`docs/model-routing.md`, "When a model call
/// fails"). Both are kernel timeouts, so no clock is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    connect: Duration,
    idle: Duration,
}

/// The production limits: 15 seconds per address to connect, 300 seconds
/// without a byte in either direction.
pub const LIMITS: Limits = Limits {
    connect: Duration::from_secs(15),
    idle: Duration::from_secs(300),
};

impl Limits {
    /// Some limit unless either bound is zero. A zero would make the
    /// socket call fail with `InvalidInput`, so it is refused here.
    pub fn new(connect: Duration, idle: Duration) -> Option<Self> {
        if connect.is_zero() || idle.is_zero() {
            None
        } else {
            Some(Self { connect, idle })
        }
    }

    /// How long one resolved address may take to connect.
    pub fn connect(&self) -> Duration {
        self.connect
    }

    /// How long one read or write syscall may wait for progress.
    pub fn idle(&self) -> Duration {
        self.idle
    }
}

impl Default for Limits {
    /// The production limits.
    fn default() -> Self {
        LIMITS
    }
}

/// Whether `error` is a socket timeout: ureq's own deadline or a socket
/// read, write or connect that waited out its bound.
#[must_use]
pub fn timed_out(error: &ureq::Error) -> bool {
    if matches!(error, ureq::Error::Timeout(_)) {
        return true;
    }
    // Only the timed-out kind is a timeout; a refused or reset peer is
    // not, and `timed_out_is_false_for_a_refused_peer` fails when this
    // guard flips to true.
    matches!(
        error,
        ureq::Error::Io(timed) if timed.kind() == io::ErrorKind::TimedOut
    )
}

/// One call's handle on its socket: keeping the handle lets another thread
/// close a read blocked inside the client, and reporting the stop keeps a
/// stopped call from opening further sockets.
pub trait Keep: std::fmt::Debug + Send + Sync + 'static {
    /// Keeps a handle to `socket`, or refuses it once the call is stopped.
    /// The refusal is decided under the same lock the stop takes, so a stop
    /// that lands between the last check and this call still closes the
    /// socket at once.
    fn keep(&self, socket: &TcpStream) -> io::Result<()>;

    /// Whether the call is stopped. No connect attempt starts after this
    /// returns true.
    fn is_stopped(&self) -> bool;
}

impl Keep for () {
    fn keep(&self, _socket: &TcpStream) -> io::Result<()> {
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        false
    }
}

/// A failed HTTPS call.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The call was stopped before a connect attempt started.
    #[error("the call was stopped")]
    Stopped,
    /// No byte arrived or was sent within the idle bound.
    #[error("no bytes for {idle:?}")]
    Stalled {
        /// The idle bound that expired.
        idle: Duration,
    },
    /// No address accepted a connection within its bound.
    #[error("could not connect to {addr} within {limit:?}")]
    ConnectTimedOut {
        /// The address that did not connect.
        addr: SocketAddr,
        /// The per-address bound that expired.
        limit: Duration,
    },
}

impl Error {
    /// The stable code a consumer switches on.
    pub fn code(&self) -> contract::ErrorCode {
        match self {
            Self::Stopped => contract::ErrorCode::ConnectionFailed,
            Self::Stalled { .. } => contract::ErrorCode::ConnectionFailed,
            Self::ConnectTimedOut { .. } => contract::ErrorCode::ConnectionFailed,
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
