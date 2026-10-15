//! The hub client's websocket transport (`docs/invocation.md`, "What the hub speaks").

#[cfg(test)]
mod frame;
#[cfg(test)]
mod handshake;
mod tls;
#[cfg(test)]
mod tunnel;

use std::sync::Arc;

/// Builds a TLS config using the same OS trust-store policy as HTTPS calls.
pub fn client_config() -> Arc<rustls::ClientConfig> {
    tls::client_config()
}

/// The connection policy for a listed hub.
#[derive(Clone)]
pub struct Via {
    /// TLS trust and client settings.
    pub tls: Arc<rustls::ClientConfig>,
    /// The HTTP CONNECT proxy, if configured.
    pub proxy: Option<ureq::Proxy>,
    /// Per-address connect and per-call idle limits.
    pub limits: crate::Limits,
    /// The name resolver, also used for IP literal hosts.
    pub resolver: Arc<dyn ureq::unversioned::resolver::Resolver>,
}

impl Via {
    /// The OS trust store, proxy environment, default resolver and production limits.
    pub fn from_env() -> Self {
        Self {
            tls: client_config(),
            proxy: ureq::Proxy::try_from_env(),
            limits: crate::LIMITS,
            resolver: Arc::new(ureq::unversioned::resolver::DefaultResolver::default()),
        }
    }
}

/// The parsed address of a hub's websocket, without credentials.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Whether to negotiate TLS using the host for verification.
    pub secure: bool,
    /// A DNS name or an IP literal, without IPv6 brackets.
    pub host: String,
    /// The TCP port.
    pub port: u16,
    /// The HTTP request target.
    pub target: String,
}
