//! The TLS configuration every HTTPS call uses, and the error its socket
//! reports (`docs/architecture.md`, "The modules", "Cancellation";
//! `docs/dependencies.md`, "Root certificates", "Proxies"). [`config`]
//! carries the platform verifier and nothing else; each caller sets its own
//! proxy and request policy on top.

use std::io;
use std::net::TcpStream;

use ureq::tls::{RootCerts, TlsConfig};

/// The TLS configuration every HTTPS call uses: the platform verifier.
pub fn tls_config() -> TlsConfig {
    TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build()
}

/// The agent config every HTTPS call starts from: [`tls_config`] and nothing
/// else. The caller sets its own proxy and request policy, so the proxy
/// default stays ureq's, read from the environment.
pub fn config() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::config::Config::builder().tls_config(tls_config())
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
}

impl Error {
    /// The stable code a consumer switches on.
    pub fn code(&self) -> contract::ErrorCode {
        match self {
            Self::Stopped => contract::ErrorCode::ConnectionFailed,
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
